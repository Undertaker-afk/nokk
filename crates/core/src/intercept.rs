//! Request interception for CDP `Fetch`: Playwright's `page.route`, Puppeteer's
//! `setRequestInterception`. While a driver intercepts, every request a page
//! makes stops before it goes out and waits for the driver's decision.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use nokk_net::{Client, HttpClient, NetError, Request, RequestKind, Response};
use tokio::sync::{mpsc, oneshot};

/// A request held before it goes out. Answer through `reply`; dropping it lets
/// the request continue unchanged.
pub struct PausedRequest {
    /// The id this request carries in every later `Network.*` record.
    pub id: String,
    pub method: String,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
    /// CDP's resource type: `Document`, `Script`, `Image`, `Fetch`, …
    pub resource_type: &'static str,
    pub reply: oneshot::Sender<Decision>,
}

pub enum Decision {
    /// Go out, with whatever the driver changed.
    Continue {
        url: Option<String>,
        method: Option<String>,
        headers: Option<BTreeMap<String, String>>,
        body: Option<Vec<u8>>,
    },
    /// Fail as a network error (`net::ERR_FAILED` to the page).
    Fail,
    /// Answer with this response; nothing goes out.
    Fulfill { status: u16, headers: BTreeMap<String, String>, body: Vec<u8> },
}

#[derive(Default)]
pub(crate) struct Interception {
    tx: Mutex<Option<mpsc::UnboundedSender<PausedRequest>>>,
    /// Ids handed out at the pause, by URL, for the record written once the
    /// request completes: the driver pairs the two by this id.
    ids: Mutex<HashMap<String, VecDeque<String>>>,
}

impl Interception {
    pub(crate) fn take_id(&self, url: &str) -> Option<String> {
        let mut ids = self.ids.lock().ok()?;
        let q = ids.get_mut(url)?;
        let id = q.pop_front();
        if q.is_empty() {
            ids.remove(url);
        }
        id
    }

    fn remember(&self, url: &str, id: String) {
        if let Ok(mut ids) = self.ids.lock() {
            ids.entry(url.to_string()).or_default().push_back(id);
        }
    }
}

/// The context's client: the shared (pooled) network client plus this page's
/// interception.
#[derive(Clone)]
pub(crate) struct PageClient {
    client: Client,
    pub(crate) hold: Arc<Interception>,
}

impl std::ops::Deref for PageClient {
    type Target = Client;
    fn deref(&self) -> &Client {
        &self.client
    }
}

fn resource_type(kind: RequestKind) -> &'static str {
    match kind {
        RequestKind::Document => "Document",
        RequestKind::Script => "Script",
        RequestKind::Xhr => "Fetch",
        RequestKind::Subresource => "Other",
    }
}

impl PageClient {
    pub(crate) fn new(client: Client) -> Self {
        PageClient { client, hold: Arc::default() }
    }

    pub(crate) fn intercept(&self) -> mpsc::UnboundedReceiver<PausedRequest> {
        let (tx, rx) = mpsc::unbounded_channel();
        if let Ok(mut slot) = self.hold.tx.lock() {
            *slot = Some(tx);
        }
        rx
    }

    pub(crate) fn stop(&self) {
        if let Ok(mut slot) = self.hold.tx.lock() {
            *slot = None;
        }
    }

    pub(crate) async fn send(&self, mut req: Request) -> Result<Response, NetError> {
        let tx = self.hold.tx.lock().ok().and_then(|t| t.clone());
        let Some(tx) = tx else {
            return self.client.send(req).await;
        };
        let id = crate::next_request_id();
        let (reply, answer) = oneshot::channel();
        let paused = PausedRequest {
            id: id.clone(),
            method: req.method.clone(),
            url: req.url.clone(),
            headers: req.headers.clone(),
            body: req.body.clone().unwrap_or_default(),
            resource_type: resource_type(req.kind),
            reply,
        };
        if tx.send(paused).is_err() {
            return self.client.send(req).await;
        }
        let decision = answer.await.unwrap_or(Decision::Continue {
            url: None,
            method: None,
            headers: None,
            body: None,
        });
        match decision {
            Decision::Continue { url, method, headers, body } => {
                if let Some(u) = url {
                    req.url = u;
                }
                if let Some(m) = method {
                    req.method = m;
                }
                if let Some(h) = headers {
                    req.headers = h;
                }
                if let Some(b) = body {
                    req.body = Some(b);
                }
                self.hold.remember(&req.url, id);
                self.client.send(req).await
            }
            Decision::Fail => {
                self.hold.remember(&req.url, id);
                Err(NetError::Connect("net::ERR_FAILED".into()))
            }
            Decision::Fulfill { status, headers, body } => {
                self.hold.remember(&req.url, id);
                Ok(Response {
                    status,
                    headers,
                    encoded_len: body.len(),
                    body,
                    elapsed_ms: 0.0,
                    content_encoding: String::new(),
                    redirect_ms: None,
                    url: req.url,
                })
            }
        }
    }
}
