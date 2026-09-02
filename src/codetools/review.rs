//! One-time, loopback-only browser code reviews for repository changes.
//!
//! `open_review` snapshots a git diff and starts a tiny HTTP server on
//! `127.0.0.1`. The page accepts one review submission; `review_status`
//! returns that structured feedback to the MCP caller.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rmcp::schemars;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use snafu::{IntoError, OptionExt, ResultExt};

use crate::error::{CodeToolSnafu, Result, ReviewEncodingSnafu, ReviewIoSnafu};

const DEFAULT_TIMEOUT_MINUTES: u16 = 60;
const MAX_TIMEOUT_MINUTES: u16 = 24 * 60;
const MAX_DIFF_BYTES: usize = 2 * 1024 * 1024;
const MAX_REQUEST_BYTES: usize = 256 * 1024;
static NEXT_REVIEW: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
pub struct OpenReviewParams {
    /// Repository root (or a directory inside it).
    pub cwd: PathBuf,
    /// Local TCP port. Omit or pass 0 to let the OS choose an available port.
    pub port: Option<u16>,
    /// Git revision to compare the working tree against (default `HEAD`).
    /// Mutually exclusive with `commit`.
    pub base: Option<String>,
    /// Review exactly this committed revision instead of the working tree.
    /// Merge commits are compared with their first parent.
    pub commit: Option<String>,
    /// Minutes before an unsubmitted page expires (default 60, maximum 1440).
    pub timeout_minutes: Option<u16>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
pub struct ReviewStatusParams {
    /// Opaque id returned by `open_review`.
    pub review_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpenReviewOutcome {
    pub review_id: String,
    pub url: String,
    pub repository: String,
    pub comparison: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    pub files_changed: usize,
    pub expires_in_minutes: u16,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum ReviewStatusOutcome {
    Pending {
        review_id: String,
        url: String,
    },
    Submitted {
        review_id: String,
        feedback: ReviewSubmission,
    },
    Expired {
        review_id: String,
    },
    Failed {
        review_id: String,
        error: String,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReviewSubmission {
    pub decision: ReviewDecision,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub comments: Vec<ReviewComment>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Approve,
    Comment,
    RequestChanges,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReviewComment {
    pub path: String,
    pub line: u32,
    pub side: DiffSide,
    pub body: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DiffSide {
    Old,
    New,
}

#[derive(Default)]
pub struct ReviewManager {
    sessions: Arc<Mutex<HashMap<String, Session>>>,
}

#[derive(Clone)]
struct Session {
    url: String,
    state: SessionState,
}

#[derive(Clone)]
enum SessionState {
    Pending,
    Submitted(ReviewSubmission),
    Expired,
    Failed(String),
}

pub fn open_review(
    tools: &super::CodeTools,
    params: OpenReviewParams,
) -> Result<OpenReviewOutcome> {
    tools.reviews.open(params)
}

pub fn review_status(
    tools: &super::CodeTools,
    params: ReviewStatusParams,
) -> Result<ReviewStatusOutcome> {
    tools.reviews.status(params)
}

impl ReviewManager {
    fn open(&self, params: OpenReviewParams) -> Result<OpenReviewOutcome> {
        let timeout = params.timeout_minutes.unwrap_or(DEFAULT_TIMEOUT_MINUTES);
        if timeout == 0 || timeout > MAX_TIMEOUT_MINUTES {
            return CodeToolSnafu {
                message: format!("timeout_minutes must be between 1 and {MAX_TIMEOUT_MINUTES}"),
            }
            .fail();
        }
        let root = repository_root(&params.cwd)?;
        if params.base.is_some() && params.commit.is_some() {
            return CodeToolSnafu {
                message: "base and commit are mutually exclusive: use base for working-tree changes or commit for one committed revision",
            }
            .fail();
        }
        let (diff, comparison, base, commit) = if let Some(commit) = params.commit {
            if commit.trim().is_empty() {
                return CodeToolSnafu {
                    message: "commit must not be empty",
                }
                .fail();
            }
            let diff = git_commit_diff(&root, &commit)?;
            let comparison = format!("Commit {commit} against its first parent");
            (diff, comparison, None, Some(commit))
        } else {
            let base = params.base.unwrap_or_else(|| "HEAD".to_string());
            if base.trim().is_empty() {
                return CodeToolSnafu {
                    message: "base must not be empty",
                }
                .fail();
            }
            let diff = git_diff(&root, &base)?;
            let comparison = format!("Working tree compared with {base}");
            (diff, comparison, Some(base), None)
        };
        if diff.trim().is_empty() {
            return CodeToolSnafu {
                message: format!(
                    "no file changes found for `{comparison}` in `{}`",
                    root.display()
                ),
            }
            .fail();
        }
        if diff.len() > MAX_DIFF_BYTES {
            return CodeToolSnafu {
                message: format!(
                    "review diff is {} bytes; the maximum is {MAX_DIFF_BYTES}",
                    diff.len()
                ),
            }
            .fail();
        }

        let bind_address = format!("127.0.0.1:{}", params.port.unwrap_or(0));
        let listener =
            TcpListener::bind(("127.0.0.1", params.port.unwrap_or(0))).context(ReviewIoSnafu {
                operation: format!("binding the review server to {bind_address}"),
            })?;
        listener.set_nonblocking(true).context(ReviewIoSnafu {
            operation: "configuring the review listener",
        })?;
        let port = listener
            .local_addr()
            .context(ReviewIoSnafu {
                operation: "reading the review listener address",
            })?
            .port();
        let id = review_id(port);
        let url = format!("http://127.0.0.1:{port}/review/{id}");
        let repository = root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("repository")
            .to_string();
        let files_changed = diff
            .lines()
            .filter(|line| line.starts_with("diff --git "))
            .count();
        let page = render_page(&repository, &comparison, &diff);

        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                id.clone(),
                Session {
                    url: url.clone(),
                    state: SessionState::Pending,
                },
            );

        let sessions = Arc::clone(&self.sessions);
        let thread_id = id.clone();
        if let Err(error) = std::thread::Builder::new()
            .name(format!("argosy-review-{port}"))
            .spawn(move || {
                serve(
                    listener,
                    &thread_id,
                    &page,
                    Duration::from_secs(u64::from(timeout) * 60),
                    &sessions,
                )
            })
        {
            self.sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            return Err(ReviewIoSnafu {
                operation: "spawning the review server thread",
            }
            .into_error(error));
        }

        Ok(OpenReviewOutcome {
            review_id: id,
            url,
            repository,
            comparison,
            base,
            commit,
            files_changed,
            expires_in_minutes: timeout,
        })
    }

    fn status(&self, params: ReviewStatusParams) -> Result<ReviewStatusOutcome> {
        let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        let session = sessions.get(&params.review_id).context(CodeToolSnafu {
            message: format!("unknown review id `{}`", params.review_id),
        })?;
        Ok(match &session.state {
            SessionState::Pending => ReviewStatusOutcome::Pending {
                review_id: params.review_id,
                url: session.url.clone(),
            },
            SessionState::Submitted(feedback) => ReviewStatusOutcome::Submitted {
                review_id: params.review_id,
                feedback: feedback.clone(),
            },
            SessionState::Expired => ReviewStatusOutcome::Expired {
                review_id: params.review_id,
            },
            SessionState::Failed(error) => ReviewStatusOutcome::Failed {
                review_id: params.review_id,
                error: error.clone(),
            },
        })
    }
}

fn repository_root(cwd: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context(ReviewIoSnafu {
            operation: format!("running git in `{}`", cwd.display()),
        })?;
    if !output.status.success() {
        return CodeToolSnafu {
            message: format!(
                "`{}` is not inside a git repository: {}",
                cwd.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        }
        .fail();
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim(),
    ))
}

fn git_diff(root: &Path, base: &str) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "diff",
            "--no-ext-diff",
            "--no-color",
            "--find-renames",
            base,
            "--",
        ])
        .output()
        .context(ReviewIoSnafu {
            operation: format!("generating the working-tree diff from `{base}`"),
        })?;
    if !output.status.success() {
        return CodeToolSnafu {
            message: format!(
                "cannot diff revision `{base}`: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        }
        .fail();
    }
    String::from_utf8(output.stdout).context(ReviewEncodingSnafu {
        artifact: "git working-tree diff",
    })
}

fn git_commit_diff(root: &Path, commit: &str) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "show",
            "--format=",
            "--first-parent",
            "--root",
            "--no-ext-diff",
            "--no-color",
            "--find-renames",
            commit,
            "--",
        ])
        .output()
        .context(ReviewIoSnafu {
            operation: format!("generating the diff for commit `{commit}`"),
        })?;
    if !output.status.success() {
        return CodeToolSnafu {
            message: format!(
                "cannot show commit `{commit}`: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        }
        .fail();
    }
    String::from_utf8(output.stdout).context(ReviewEncodingSnafu {
        artifact: "git commit diff",
    })
}

fn review_id(port: u16) -> String {
    let mut hasher = Sha256::new();
    hasher.update(std::process::id().to_le_bytes());
    hasher.update(port.to_le_bytes());
    hasher.update(NEXT_REVIEW.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    hasher.update(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .to_le_bytes(),
    );
    hasher.finalize()[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn serve(
    listener: TcpListener,
    id: &str,
    page: &str,
    timeout: Duration,
    sessions: &Arc<Mutex<HashMap<String, Session>>>,
) {
    let deadline = Instant::now() + timeout;
    let page_path = format!("/review/{id}");
    let submit_path = format!("{page_path}/submit");
    while Instant::now() < deadline {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let request = match read_request(&mut stream) {
                    Ok(request) => request,
                    Err(message) => {
                        respond_best_effort(
                            &mut stream,
                            "400 Bad Request",
                            "text/plain; charset=utf-8",
                            &message,
                        );
                        continue;
                    }
                };
                if request.method == "GET" && request.path == page_path {
                    respond_best_effort(&mut stream, "200 OK", "text/html; charset=utf-8", page);
                } else if request.method == "POST" && request.path == submit_path {
                    match serde_json::from_slice::<ReviewSubmission>(&request.body)
                        .and_then(validate_submission)
                    {
                        Ok(feedback) => {
                            if let Some(session) = sessions
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .get_mut(id)
                            {
                                session.state = SessionState::Submitted(feedback);
                            }
                            respond_best_effort(
                                &mut stream,
                                "200 OK",
                                "application/json",
                                r#"{"submitted":true}"#,
                            );
                            return;
                        }
                        Err(error) => {
                            let body = serde_json::json!({"error": error.to_string()}).to_string();
                            respond_best_effort(
                                &mut stream,
                                "400 Bad Request",
                                "application/json",
                                &body,
                            )
                        }
                    }
                } else {
                    respond_best_effort(
                        &mut stream,
                        "404 Not Found",
                        "text/plain; charset=utf-8",
                        "not found",
                    );
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error) => {
                if let Some(session) = sessions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get_mut(id)
                {
                    session.state =
                        SessionState::Failed(format!("review listener failed: {error}"));
                }
                return;
            }
        }
    }
    if let Some(session) = sessions
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_mut(id)
        && matches!(session.state, SessionState::Pending)
    {
        session.state = SessionState::Expired;
    }
}

struct HttpRequest {
    method: String,
    path: String,
    body: Vec<u8>,
}

fn read_request(stream: &mut TcpStream) -> std::result::Result<HttpRequest, String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut buffer).map_err(|e| e.to_string())?;
        if read == 0 {
            return Err("incomplete request".into());
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err("request is too large".into());
        }
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let header = std::str::from_utf8(&bytes[..header_end]).map_err(|_| "invalid headers")?;
    let mut lines = header.lines();
    let mut request_line = lines
        .next()
        .ok_or("missing request line")?
        .split_whitespace();
    let method = request_line.next().ok_or("missing method")?.to_string();
    let path = request_line.next().ok_or("missing path")?.to_string();
    let content_length = lines
        .find_map(|line| {
            line.split_once(':').and_then(|(name, value)| {
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
        })
        .unwrap_or(0);
    if header_end + content_length > MAX_REQUEST_BYTES {
        return Err("request is too large".into());
    }
    while bytes.len() < header_end + content_length {
        let read = stream.read(&mut buffer).map_err(|e| e.to_string())?;
        if read == 0 {
            return Err("incomplete request body".into());
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    Ok(HttpRequest {
        method,
        path,
        body: bytes[header_end..header_end + content_length].to_vec(),
    })
}

fn validate_submission(
    submission: ReviewSubmission,
) -> std::result::Result<ReviewSubmission, serde_json::Error> {
    if submission.comments.iter().any(|comment| {
        comment.path.trim().is_empty()
            || comment.line == 0
            || comment.body.trim().is_empty()
            || comment.body.len() > 10_000
    }) || submission.summary.len() > 50_000
    {
        return Err(serde::de::Error::custom(
            "invalid or oversized review feedback",
        ));
    }
    Ok(submission)
}

fn respond(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'self'\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

fn respond_best_effort(stream: &mut TcpStream, status: &str, content_type: &str, body: &str) {
    if let Err(error) = respond(stream, status, content_type, body) {
        // A disconnected browser is local to this request. It must not fail
        // the still-live review session, but retaining a diagnostic makes the
        // intentionally swallowed connection error observable to hosts that
        // install a tracing subscriber.
        tracing::debug!(%error, %status, "review HTTP response failed");
    }
}

fn render_page(repository: &str, comparison: &str, diff: &str) -> String {
    let rows = render_diff(diff);
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Review {repository}</title><style>
:root{{--bg:#0d1117;--panel:#161b22;--border:#30363d;--text:#e6edf3;--muted:#8b949e;--add:#12261e;--del:#301b1e;--blue:#2f81f7}}*{{box-sizing:border-box}}body{{margin:0;background:var(--bg);color:var(--text);font:14px -apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif}}header{{position:sticky;top:0;z-index:2;padding:16px 24px;background:rgba(13,17,23,.96);border-bottom:1px solid var(--border)}}h1{{font-size:20px;margin:0 0 4px}}.muted{{color:var(--muted)}}main{{max-width:1400px;margin:20px auto;padding:0 20px 300px}}.file{{border:1px solid var(--border);border-radius:7px;margin:16px 0;overflow:hidden}}.file-title{{padding:10px 14px;background:var(--panel);font-family:ui-monospace,monospace;font-weight:600}}table{{border-collapse:collapse;width:100%;font:12px ui-monospace,SFMono-Regular,Consolas,monospace}}td{{vertical-align:top}}.num{{width:52px;text-align:right;color:var(--muted);padding:0 8px;border-right:1px solid var(--border);user-select:none}}.code{{white-space:pre-wrap;word-break:break-all;padding:0 10px}}tr.add{{background:var(--add)}}tr.del{{background:var(--del)}}tr.meta .code{{color:var(--muted)}}tr.commentable{{cursor:pointer}}tr.commentable:hover{{outline:1px solid var(--blue);outline-offset:-1px}}aside{{position:fixed;z-index:3;right:20px;bottom:20px;width:min(480px,calc(100vw - 40px));max-height:70vh;overflow:auto;background:var(--panel);border:1px solid var(--border);border-radius:10px;padding:16px;box-shadow:0 12px 40px #0008}}textarea{{width:100%;min-height:70px;margin:8px 0;padding:9px;background:var(--bg);color:var(--text);border:1px solid var(--border);border-radius:6px}}button,select{{padding:8px 12px;border-radius:6px;border:1px solid var(--border);background:#21262d;color:var(--text)}}button.primary{{background:#238636;border-color:#2ea043;font-weight:600}}.comment{{border-top:1px solid var(--border);padding-top:8px;margin-top:8px}}.comment strong{{font:12px ui-monospace,monospace}}.remove{{float:right;color:#f85149}}#done{{display:none;color:#3fb950;font-weight:600}}</style></head>
<body><header><h1>{repository}</h1><span class="muted"><code>{comparison}</code> · Click a changed or context line to comment</span></header><main>{rows}</main>
<aside><h2>Submit review</h2><div id="comments"></div><label>Summary<textarea id="summary" placeholder="Overall feedback"></textarea></label><select id="decision"><option value="comment">Comment</option><option value="approve">Approve</option><option value="request_changes">Request changes</option></select> <button class="primary" id="submit">Submit review</button><span id="done">Review submitted. You can close this page.</span></aside>
<script>const comments=document.querySelector('#comments');document.querySelectorAll('tr.commentable').forEach(r=>r.onclick=()=>{{const key=r.dataset.path+':'+r.dataset.line+':'+r.dataset.side;if(document.querySelector(`[data-key="${{CSS.escape(key)}}"]`))return;const d=document.createElement('div');d.className='comment';d.dataset.key=key;d.dataset.path=r.dataset.path;d.dataset.line=r.dataset.line;d.dataset.side=r.dataset.side;const s=document.createElement('strong');s.textContent=key;const x=document.createElement('button');x.className='remove';x.textContent='Remove';x.onclick=()=>d.remove();const t=document.createElement('textarea');t.placeholder='Leave a line comment';d.append(s,x,t);comments.append(d);t.focus()}});document.querySelector('#submit').onclick=async()=>{{const button=document.querySelector('#submit');button.disabled=true;const payload={{decision:document.querySelector('#decision').value,summary:document.querySelector('#summary').value,comments:[...comments.children].filter(d=>d.querySelector('textarea').value.trim()).map(d=>({{path:d.dataset.path,line:Number(d.dataset.line),side:d.dataset.side,body:d.querySelector('textarea').value}}))}};try{{const response=await fetch(location.pathname+'/submit',{{method:'POST',headers:{{'Content-Type':'application/json'}},body:JSON.stringify(payload)}});if(!response.ok)throw new Error(await response.text());document.querySelector('#done').style.display='inline';button.style.display='none';document.querySelector('#decision').disabled=true}}catch(e){{alert('Could not submit review: '+e.message);button.disabled=false}}}};</script></body></html>"#,
        repository = html_escape(repository),
        comparison = html_escape(comparison),
    )
}

fn render_diff(diff: &str) -> String {
    let mut out = String::new();
    let mut path = String::new();
    let mut old_line = 0_u32;
    let mut new_line = 0_u32;
    let mut file_open = false;
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            if file_open {
                out.push_str("</table></div>");
            }
            file_open = true;
            path = line
                .rsplit_once(" b/")
                .map(|(_, value)| value.trim_end_matches('"').to_string())
                .unwrap_or_else(|| "changed file".to_string());
            write!(
                out,
                "<div class=\"file\"><div class=\"file-title\">{}</div><table>",
                html_escape(&path)
            )
            .unwrap();
            continue;
        }
        if let Some(value) = line.strip_prefix("+++ ") {
            if value != "/dev/null" {
                path = value.strip_prefix("b/").unwrap_or(value).to_string();
            }
            diff_row(&mut out, "meta", "", "", line, None);
            continue;
        }
        if let Some(hunk) = line.strip_prefix("@@ ") {
            if let Some((range, _)) = hunk.split_once(" @@") {
                let mut ranges = range.split_whitespace();
                old_line = range_start(ranges.next().unwrap_or("-0"));
                new_line = range_start(ranges.next().unwrap_or("+0"));
            }
            diff_row(&mut out, "meta", "", "", line, None);
        } else if line.starts_with('+') && !line.starts_with("+++") {
            diff_row(
                &mut out,
                "add",
                "",
                &new_line.to_string(),
                line,
                Some((&path, new_line, "new")),
            );
            new_line += 1;
        } else if line.starts_with('-') && !line.starts_with("---") {
            diff_row(
                &mut out,
                "del",
                &old_line.to_string(),
                "",
                line,
                Some((&path, old_line, "old")),
            );
            old_line += 1;
        } else if line.starts_with(' ') {
            diff_row(
                &mut out,
                "context",
                &old_line.to_string(),
                &new_line.to_string(),
                line,
                Some((&path, new_line, "new")),
            );
            old_line += 1;
            new_line += 1;
        } else if file_open && !path.is_empty() {
            diff_row(&mut out, "meta", "", "", line, None);
        }
    }
    if file_open {
        out.push_str("</table></div>");
    }
    out
}

fn range_start(range: &str) -> u32 {
    range
        .trim_start_matches(['-', '+'])
        .split(',')
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

fn diff_row(
    out: &mut String,
    class: &str,
    old: &str,
    new: &str,
    code: &str,
    target: Option<(&str, u32, &str)>,
) {
    let target = target.map_or_else(String::new, |(path, line, side)| {
        format!(
            " commentable\" data-path=\"{}\" data-line=\"{line}\" data-side=\"{side}",
            html_escape(path)
        )
    });
    write!(
        out,
        "<tr class=\"{class}{target}\"><td class=\"num\">{old}</td><td class=\"num\">{new}</td><td class=\"code\">{}</td></tr>",
        html_escape(code)
    )
    .unwrap();
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    fn request(address: &str, request: &str) -> String {
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            if let Some(expected) = http_response_length(&response)
                && response.len() >= expected
            {
                response.truncate(expected);
                break;
            }
            match stream.read(&mut buffer) {
                Ok(0) => panic!("HTTP response ended before its declared Content-Length"),
                Ok(read) => response.extend_from_slice(&buffer[..read]),
                Err(error) => panic!("failed to read HTTP response: {error}"),
            }
        }
        String::from_utf8(response).unwrap()
    }

    fn http_response_length(response: &[u8]) -> Option<usize> {
        let header_end = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")?
            + 4;
        let header = std::str::from_utf8(&response[..header_end]).ok()?;
        let content_length = header.lines().find_map(|line| {
            line.split_once(':').and_then(|(name, value)| {
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
        })?;
        Some(header_end + content_length)
    }

    #[test]
    fn renders_commentable_diff_without_interpreting_source_as_html() {
        let diff =
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-<old>\n+<new>\n";
        let html = render_diff(diff);
        assert!(html.contains("data-path=\"a.rs\" data-line=\"1\" data-side=\"new\""));
        assert!(html.contains("&lt;new&gt;"));
        assert!(!html.contains("<new>"));
    }

    #[test]
    fn renders_binary_changes_in_their_own_file_section() {
        let diff = "diff --git a/logo.png b/logo.png\nindex 111..222 100644\nBinary files a/logo.png and b/logo.png differ\n";
        let html = render_diff(diff);
        assert!(html.contains("file-title\">logo.png"));
        assert!(html.contains("Binary files a/logo.png and b/logo.png differ"));
    }

    #[test]
    fn submission_validation_rejects_empty_line_comments() {
        let submission = ReviewSubmission {
            decision: ReviewDecision::Comment,
            summary: String::new(),
            comments: vec![ReviewComment {
                path: "src/lib.rs".into(),
                line: 1,
                side: DiffSide::New,
                body: " ".into(),
            }],
        };
        assert!(validate_submission(submission).is_err());
    }

    #[test]
    fn commit_mode_isolates_the_selected_commit() {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "--quiet"]);
        git(repo.path(), &["config", "user.email", "review@example.com"]);
        git(repo.path(), &["config", "user.name", "Review Test"]);
        std::fs::write(repo.path().join("value.txt"), "one\n").unwrap();
        git(repo.path(), &["add", "value.txt"]);
        git(repo.path(), &["commit", "--quiet", "-m", "one"]);
        std::fs::write(repo.path().join("value.txt"), "two\n").unwrap();
        git(repo.path(), &["commit", "--quiet", "-am", "two"]);
        let selected = Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let selected = String::from_utf8(selected.stdout).unwrap();
        let selected = selected.trim();
        std::fs::write(repo.path().join("value.txt"), "three\n").unwrap();
        git(repo.path(), &["commit", "--quiet", "-am", "three"]);

        let diff = git_commit_diff(repo.path(), selected).unwrap();
        assert!(diff.contains("+two"));
        assert!(diff.contains("-one"));
        assert!(!diff.contains("three"));
    }

    #[test]
    fn bind_failure_preserves_its_io_source() {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "--quiet"]);
        git(repo.path(), &["config", "user.email", "review@example.com"]);
        git(repo.path(), &["config", "user.name", "Review Test"]);
        std::fs::write(repo.path().join("value.txt"), "one\n").unwrap();
        git(repo.path(), &["add", "value.txt"]);
        git(repo.path(), &["commit", "--quiet", "-m", "one"]);
        std::fs::write(repo.path().join("value.txt"), "two\n").unwrap();
        let occupied = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = occupied.local_addr().unwrap().port();

        let error = open_review(
            &super::super::CodeTools::default(),
            OpenReviewParams {
                cwd: repo.path().to_path_buf(),
                port: Some(port),
                base: None,
                commit: None,
                timeout_minutes: Some(1),
            },
        )
        .unwrap_err();
        match error {
            crate::error::Error::ReviewIo { operation, source } => {
                assert!(operation.contains("binding"));
                assert_eq!(source.kind(), std::io::ErrorKind::AddrInUse);
            }
            other => panic!("expected review I/O context, got {other:?}"),
        }
    }

    #[test]
    fn browser_submission_is_returned_to_the_caller() {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "--quiet"]);
        git(repo.path(), &["config", "user.email", "review@example.com"]);
        git(repo.path(), &["config", "user.name", "Review Test"]);
        std::fs::write(repo.path().join("hello.txt"), "before\n").unwrap();
        git(repo.path(), &["add", "hello.txt"]);
        git(repo.path(), &["commit", "--quiet", "-m", "initial"]);
        std::fs::write(repo.path().join("hello.txt"), "after\n").unwrap();

        let tools = super::super::CodeTools::default();
        let opened = open_review(
            &tools,
            OpenReviewParams {
                cwd: repo.path().to_path_buf(),
                port: Some(0),
                base: None,
                commit: None,
                timeout_minutes: Some(1),
            },
        )
        .unwrap();
        let address = opened
            .url
            .strip_prefix("http://")
            .unwrap()
            .split('/')
            .next()
            .unwrap();
        let path = format!("/review/{}", opened.review_id);
        let page = request(
            address,
            &format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"),
        );
        assert!(
            page.starts_with("HTTP/1.1 200 OK"),
            "unexpected response: {page:?}"
        );
        assert!(page.contains("hello.txt"));

        let body = r#"{"decision":"request_changes","summary":"Please revise.","comments":[{"path":"hello.txt","line":1,"side":"new","body":"Keep the greeting."}]}"#;
        let response = request(
            address,
            &format!(
                "POST {path}/submit HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ),
        );
        assert!(response.starts_with("HTTP/1.1 200 OK"));

        let status = review_status(
            &tools,
            ReviewStatusParams {
                review_id: opened.review_id,
            },
        )
        .unwrap();
        match status {
            ReviewStatusOutcome::Submitted { feedback, .. } => {
                assert!(matches!(feedback.decision, ReviewDecision::RequestChanges));
                assert_eq!(feedback.comments[0].path, "hello.txt");
                assert_eq!(feedback.comments[0].line, 1);
            }
            other => panic!("expected submitted review, got {other:?}"),
        }
    }
}
