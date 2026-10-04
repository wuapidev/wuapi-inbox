//! The two stand-ins for the stream: a local server on a real socket, for
//! what only HTTP can show, and an in-memory transport on the paused clock,
//! for the link's state machine. A paused clock advances by itself while
//! real socket I/O is pending, so timeouts would fire at random: the
//! scripted transport has no sockets.

// The stand-ins are shared by the later units, which use what is spare here.
#![allow(dead_code)]

use crate::stream::{Answer, Refused, StreamBody, StreamTransport};
use async_trait::async_trait;
use futures::future::BoxFuture;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// One thing a stream does.
#[derive(Clone, Debug)]
pub(crate) enum Step {
    /// Sends these bytes.
    Write(Vec<u8>),
    Sleep(Duration),
    /// Sends nothing more and keeps the connection open.
    Hang,
    /// Ends the body cleanly.
    Close,
    /// Breaks the connection (scripted transport only).
    Fail(String),
}

pub(crate) fn write(text: &str) -> Step {
    Step::Write(text.as_bytes().to_vec())
}

/// How one connection to [`FakeStream`] is answered.
#[derive(Clone, Debug)]
pub(crate) struct Script {
    status: u16,
    content_type: &'static str,
    headers: Vec<(String, String)>,
    /// A stream when `None`; otherwise a short body, then close.
    body: Option<Vec<u8>>,
    steps: Vec<Step>,
}

impl Script {
    /// `200 text/event-stream`, then the steps.
    pub(crate) fn stream(steps: Vec<Step>) -> Self {
        Self {
            status: 200,
            content_type: "text/event-stream",
            headers: Vec::new(),
            body: None,
            steps,
        }
    }

    /// A plain answer with a body, then close.
    pub(crate) fn answer(status: u16, content_type: &'static str, body: &str) -> Self {
        Self {
            status,
            content_type,
            headers: Vec::new(),
            body: Some(body.as_bytes().to_vec()),
            steps: Vec::new(),
        }
    }

    pub(crate) fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }
}

/// A server on `127.0.0.1` that answers each connection with the next
/// script and records the request head it got. Close-delimited bodies.
pub(crate) struct FakeStream {
    address: std::net::SocketAddr,
    heads: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}

impl FakeStream {
    pub(crate) async fn start(scripts: Vec<Script>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let heads = Arc::new(Mutex::new(Vec::new()));
        let recorded = heads.clone();
        let scripts = Arc::new(Mutex::new(VecDeque::from(scripts)));
        let task = tokio::spawn(async move {
            let mut handlers = Vec::new();
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    break;
                };
                let script = scripts.lock().unwrap().pop_front();
                let recorded = recorded.clone();
                handlers.push(tokio::spawn(serve(socket, script, recorded)));
            }
        });
        Self {
            address,
            heads,
            task,
        }
    }

    /// `http://127.0.0.1:port`
    pub(crate) fn uri(&self) -> String {
        format!("http://{}", self.address)
    }

    /// The request heads received, one string per connection.
    pub(crate) fn heads(&self) -> Vec<String> {
        self.heads.lock().unwrap().clone()
    }
}

impl Drop for FakeStream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(
    mut socket: tokio::net::TcpStream,
    script: Option<Script>,
    heads: Arc<Mutex<Vec<String>>>,
) {
    // The head first: closing with it unread resets the connection and
    // cuts the body short.
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        match socket.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => head.extend_from_slice(&buf[..n]),
        }
    }
    heads
        .lock()
        .unwrap()
        .push(String::from_utf8_lossy(&head).into_owned());
    let Some(script) = script else {
        return;
    };
    let mut answer = format!(
        "HTTP/1.1 {} X\r\nContent-Type: {}\r\nConnection: close\r\n",
        script.status, script.content_type
    );
    for (name, value) in &script.headers {
        answer.push_str(&format!("{name}: {value}\r\n"));
    }
    if let Some(body) = &script.body {
        answer.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    answer.push_str("\r\n");
    if socket.write_all(answer.as_bytes()).await.is_err() {
        return;
    }
    if let Some(body) = &script.body {
        let _ = socket.write_all(body).await;
        let _ = socket.shutdown().await;
        return;
    }
    for step in script.steps {
        match step {
            Step::Write(bytes) => {
                if socket.write_all(&bytes).await.is_err() || socket.flush().await.is_err() {
                    return;
                }
            }
            Step::Sleep(wait) => tokio::time::sleep(wait).await,
            Step::Hang => std::future::pending::<()>().await,
            Step::Close | Step::Fail(_) => break,
        }
    }
    let _ = socket.shutdown().await;
}

// ----- the scripted transport ------------------------------------------------

/// What one scripted connect does.
#[derive(Clone, Debug)]
pub(crate) enum Attempt {
    /// No answer at all: a network failure.
    Fail(String),
    /// An answer that is not the stream.
    Refuse(Refused),
    /// A stream that plays these steps.
    Stream(Vec<Step>),
}

/// A refusal with a JSON body and, maybe, `Retry-After`.
pub(crate) fn refused(status: u16, body: &str, retry_after: Option<u64>) -> Attempt {
    Attempt::Refuse(Refused {
        status,
        content_type: Some("application/json".into()),
        body: body.as_bytes().to_vec(),
        retry_after: retry_after.map(Duration::from_secs),
    })
}

/// The `Last-Event-ID` and the instant of one connect.
pub(crate) type Connect = (Option<String>, Instant);

/// In-memory stand-in for the stream, with no socket: connects are played
/// from a list, and each is recorded with its `Last-Event-ID` and instant.
#[derive(Clone, Default)]
pub(crate) struct ScriptedTransport {
    attempts: Arc<Mutex<VecDeque<Attempt>>>,
    connects: Arc<Mutex<Vec<Connect>>>,
}

impl ScriptedTransport {
    pub(crate) fn new(attempts: Vec<Attempt>) -> Self {
        Self {
            attempts: Arc::new(Mutex::new(attempts.into())),
            connects: Arc::default(),
        }
    }

    /// Queues one more connect.
    pub(crate) fn push(&self, attempt: Attempt) {
        self.attempts.lock().unwrap().push_back(attempt);
    }

    /// The `Last-Event-ID` and the time of every connect so far.
    pub(crate) fn connects(&self) -> Vec<Connect> {
        self.connects.lock().unwrap().clone()
    }
}

impl StreamTransport for ScriptedTransport {
    fn connect(&self, last_event_id: Option<String>) -> BoxFuture<'static, Result<Answer, String>> {
        self.connects
            .lock()
            .unwrap()
            .push((last_event_id, Instant::now()));
        let next = self.attempts.lock().unwrap().pop_front();
        Box::pin(async move {
            match next {
                None => Err("the script has no more connects".to_owned()),
                Some(Attempt::Fail(why)) => Err(why),
                Some(Attempt::Refuse(refused)) => Ok(Answer::Refused(refused)),
                Some(Attempt::Stream(steps)) => Ok(Answer::Stream(Box::new(ScriptedBody {
                    steps: steps.into(),
                    until: None,
                }))),
            }
        })
    }
}

struct ScriptedBody {
    steps: VecDeque<Step>,
    /// When the sleep being waited out ends: kept here, so a dropped
    /// `chunk` future loses neither the step nor the time already waited.
    until: Option<Instant>,
}

#[async_trait]
impl StreamBody for ScriptedBody {
    async fn chunk(&mut self) -> Result<Option<Vec<u8>>, String> {
        loop {
            match self.steps.front() {
                None | Some(Step::Close) => return Ok(None),
                Some(Step::Hang) => std::future::pending::<()>().await,
                Some(Step::Fail(why)) => return Err(why.clone()),
                Some(Step::Write(_)) => {
                    let Some(Step::Write(bytes)) = self.steps.pop_front() else {
                        unreachable!()
                    };
                    return Ok(Some(bytes));
                }
                Some(Step::Sleep(wait)) => {
                    let until = *self.until.get_or_insert_with(|| Instant::now() + *wait);
                    tokio::time::sleep_until(until).await;
                    self.until = None;
                    self.steps.pop_front();
                }
            }
        }
    }
}
