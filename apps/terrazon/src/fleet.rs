//! The fleet feeds' HTTP: one HTTPS GET per ask with the feed's own auth headers (Bolt's
//! `Authorization: Bearer …`, the radars' `Cookie`), answered on the channel the UI's fleet
//! runtime polls. A poll is one to three GETs every few seconds — a small dedicated thread per
//! ask, no pool.

use std::io::Read;
use std::sync::mpsc::Sender;

use terrazon_fleet::{FleetFetch, FleetMsg};

/// The fleet fetcher over ureq (the `UreqTiles` shape, minus the pool and the disk cache —
/// fleet answers are live data, never cached).
#[derive(Default)]
pub struct UreqFleet;

impl UreqFleet {
    pub fn start() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self)
    }
}

/// The error string a failed fleet HTTP carries: the status code AND the vendor's own
/// response body — a 400 that says "session expired" reads like a reason, a bare URL
/// doesn't. Transport errors (dns, connect) keep ureq's own text.
fn fleet_http_error(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, resp) => {
            let mut body = String::new();
            let _ = resp.into_reader().take(8 << 10).read_to_string(&mut body);
            let body = body.trim();
            if body.is_empty() { format!("status code {code}") } else { format!("status code {code}: {}", &body[..body.len().min(400)]) }
        }
        other => other.to_string(),
    }
}

impl FleetFetch for UreqFleet {
    fn get(&self, key: String, url: String, headers: Vec<(String, String)>, tx: Sender<FleetMsg>) {
        // The thread takes the key and the sender; a spare pair answers if the thread can't start.
        let (tx2, key2) = (tx.clone(), key.clone());
        let name = format!("fleet-{key}");
        let spawned = std::thread::Builder::new().name(name).spawn(move || {
            let agent: ureq::Agent =
                crate::http::agent_builder().timeout_connect(std::time::Duration::from_secs(8)).timeout(std::time::Duration::from_secs(15)).build();
            let answer = (|| -> Result<(Vec<u8>, Option<String>), String> {
                // Only https: the feeds carry credentials.
                if !url.starts_with("https://") {
                    return Err("the fleet endpoint must be https".into());
                }
                let mut req = agent.get(&url);
                for (k, v) in &headers {
                    req = req.set(k, v);
                }
                let resp = req.call().map_err(fleet_http_error)?;
                // The system-managed radars bootstrap their session here: whatever cookies the
                // answer sets ride back to the runtime (which stores them via fleet.systemAuth).
                let set_cookie = {
                    let all = resp.all("set-cookie");
                    (!all.is_empty()).then(|| all.join("; "))
                };
                let mut buf = Vec::new();
                resp.into_reader().take(8 << 20).read_to_end(&mut buf).map_err(|e| e.to_string())?;
                Ok((buf, set_cookie))
            })();
            let _ = tx.send(match answer {
                Ok((bytes, set_cookie)) => FleetMsg::Done { key, bytes, set_cookie },
                Err(reason) => FleetMsg::Failed { key, reason },
            });
        });
        if spawned.is_err() {
            // No thread: answer failed so the ask isn't left pending forever.
            let _ = tx2.send(FleetMsg::Failed { key: key2, reason: "the fetcher thread could not start".into() });
        }
    }

    fn post(&self, key: String, url: String, headers: Vec<(String, String)>, body: String, content_type: String, tx: Sender<FleetMsg>) {
        // The same dedicated-thread shape as `get`: Bolt's sign-in and vehicle poll ride
        // this, and their answers are small JSON — a 1 MB cap is generous.
        let (tx2, key2) = (tx.clone(), key.clone());
        let name = format!("fleet-{key}");
        let spawned = std::thread::Builder::new().name(name).spawn(move || {
            let agent: ureq::Agent =
                crate::http::agent_builder().timeout_connect(std::time::Duration::from_secs(8)).timeout(std::time::Duration::from_secs(15)).build();
            let answer = (|| -> Result<(Vec<u8>, Option<String>), String> {
                if !url.starts_with("https://") {
                    return Err("the fleet endpoint must be https".into());
                }
                let mut req = agent.post(&url).set("Content-Type", &content_type);
                for (k, v) in &headers {
                    if k.eq_ignore_ascii_case("content-type") {
                        continue;
                    }
                    req = req.set(k, v);
                }
                let resp = req.send_string(&body).map_err(fleet_http_error)?;
                let set_cookie = {
                    let all = resp.all("set-cookie");
                    (!all.is_empty()).then(|| all.join("; "))
                };
                let mut buf = Vec::new();
                resp.into_reader().take(1 << 20).read_to_end(&mut buf).map_err(|e| e.to_string())?;
                Ok((buf, set_cookie))
            })();
            let _ = tx.send(match answer {
                Ok((bytes, set_cookie)) => FleetMsg::Done { key, bytes, set_cookie },
                Err(reason) => FleetMsg::Failed { key, reason },
            });
        });
        if spawned.is_err() {
            let _ = tx2.send(FleetMsg::Failed { key: key2, reason: "the fetcher thread could not start".into() });
        }
    }
}
