//! What's waiting on you, for the palette's Answer… and Answer Next…, which ask it in Rex's
//! own palette: an approval, or an agent's questions with their options. `waiting` prints the
//! focused thread's request, else the next one on any machine, as JSON the Lua reads; `respond`
//! sends the answer once it has checked the request is still waiting, since another device may
//! have answered it while the palette was open.

use anyhow::{Result, bail};
use serde_json::{Value, json};
use t3client::projection::ThreadState;

use crate::ask;
use crate::state::State;
use crate::t3;

/// Where `key` (`env/thread`) has a tab, as Rex's `session.select` takes it.
fn tab(state: &State, key: &str) -> Value {
    state
        .groups
        .values()
        .find_map(|g| {
            let window = g.windows.get(key)?;
            Some(json!({"session_id": g.session_id.clone()?, "window_id": window.window_id}))
        })
        .unwrap_or(Value::Null)
}

/// Each thread that might be waiting, focused first, then the one ⌃⌘I goes to, then every
/// other thread on every machine with a request for the user, newest first.
async fn candidates(focused: Option<String>, state: &State) -> Vec<String> {
    let mut keys: Vec<String> = focused.into_iter().collect();
    let next = state
        .next
        .as_ref()
        .and_then(|n| n.window_id.as_deref())
        .and_then(|window| {
            state.groups.values().find_map(|g| {
                g.windows
                    .iter()
                    .find(|(_, w)| w.window_id == window)
                    .map(|(k, _)| k.clone())
            })
        });
    keys.extend(next);
    let hub = t3::hub();
    let mut asking: Vec<(String, String)> = Vec::new();
    for env in hub.envs() {
        let Ok(Ok(shell)) =
            tokio::time::timeout(std::time::Duration::from_secs(5), env.shell()).await
        else {
            continue;
        };
        for thread in &shell.threads {
            if t3client::inbox::pending_kind(thread).is_none() {
                continue;
            }
            let Some(id) = thread["id"].as_str() else {
                continue;
            };
            let at = thread["updatedAt"].as_str().unwrap_or_default().to_string();
            asking.push((at, format!("{}/{id}", env.id())));
        }
    }
    asking.sort_by(|a, b| b.0.cmp(&a.0));
    keys.extend(asking.into_iter().map(|(_, k)| k));
    let mut seen = std::collections::HashSet::new();
    keys.retain(|k| seen.insert(k.clone()));
    keys
}

/// The palette's view of what `state` waits on, or None.
fn describe(key: &str, state: &ThreadState, tab: Value) -> Option<Value> {
    let title = crate::sync::label(state.thread()["title"].as_str().unwrap_or(""));
    let title = if title.is_empty() {
        "Untitled".to_string()
    } else {
        title
    };
    let mut out = json!({"thread": key, "title": title, "tab": tab});
    let Some((request, item)) = ask::pending(state) else {
        let secret = ask::pending_secret(state)?;
        out["kind"] = json!("secret");
        out["what"] = json!(ask::clean(secret["label"].as_str().unwrap_or("A secret")));
        return Some(out);
    };
    out["request"] = request["id"].clone();
    if ask::is_question(&request) {
        out["kind"] = json!("question");
        out["questions"] = Value::Array(
            item["questions"]
                .as_array()
                .into_iter()
                .flatten()
                .map(question)
                .collect(),
        );
    } else {
        let (verb, what) = ask::approval(state, &request, &item);
        out["kind"] = json!("approval");
        out["verb"] = json!(verb);
        out["what"] = json!(what);
    }
    Some(out)
}

/// One question as `rex.ui.pick` takes it: its options as items, each once, by the value T3
/// takes back.
fn question(q: &Value) -> Value {
    let mut ids = std::collections::HashSet::new();
    let options: Vec<Value> = q["options"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| {
            let label = o["label"].as_str()?;
            let id = o["value"].as_str().unwrap_or(label);
            if !ids.insert(id.to_string()) {
                return None;
            }
            let mut item = json!({"id": id, "title": ask::clean(label)});
            if let Some(d) = o["description"].as_str().filter(|d| !d.trim().is_empty()) {
                item["subtitle"] = json!(ask::clean(d));
            }
            Some(item)
        })
        .collect();
    let header = q["header"]
        .as_str()
        .map(ask::clean)
        .filter(|h| !h.is_empty());
    json!({
        "id": q["id"],
        "header": header,
        "question": ask::question_text(q),
        "multiple": q["multiSelect"] == true,
        "options": options,
    })
}

/// `waiting`: what the focused thread waits on, else the next thread that does, leaving out
/// `skip`. With `only_focused`, the focused thread's alone. Prints `null` for nothing.
pub async fn print(focused: Option<String>, only_focused: bool, skip: &[String]) -> Result<()> {
    let state = State::load();
    let keys = if only_focused {
        focused.into_iter().collect()
    } else {
        candidates(focused, &state).await
    };
    for key in keys.iter().filter(|k| !skip.contains(k)) {
        let Ok((env, r)) = t3::thread(key) else {
            continue;
        };
        let Ok(thread) = env.thread(&r.thread, true).await else {
            continue;
        };
        if let Some(found) = describe(key, &thread, tab(&state, key)) {
            println!("{found}");
            return Ok(());
        }
    }
    println!("null");
    Ok(())
}

/// `respond`: answers `request` on `key`, an approval with `decision`, else questions with
/// `answers` (`{question id: value or [values]}`), once it's still the one waiting.
pub async fn respond(
    key: &str,
    request: &str,
    decision: Option<&str>,
    answers: Option<&str>,
) -> Result<()> {
    let (env, r) = t3::thread(key)?;
    let state = env.thread(&r.thread, true).await?;
    let still = ask::pending(&state).is_some_and(|(p, _)| p["id"] == request);
    if !still {
        bail!("That was already answered");
    }
    match (decision, answers) {
        (Some(decision), None) => env.respond(&r.thread, request, decision).await?,
        (None, Some(answers)) => {
            let answers: Value = serde_json::from_str(answers)?;
            env.answer(&r.thread, request, answers).await?
        }
        _ => bail!("give a decision or answers"),
    };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(requests: Value, items: Value) -> ThreadState {
        ThreadState::from_snapshot(&json!({
            "kind": "snapshot",
            "snapshotSequence": 1,
            "projection": {
                "thread": {"id": "t1", "title": "Ship it"},
                "runtimeRequests": requests,
                "turnItems": items,
                "nodes": [],
            }
        }))
        .unwrap()
    }

    #[test]
    fn a_question_lists_its_options_once() {
        let s = state(
            json!([{"id": "r1", "status": "pending", "kind": "user_input"}]),
            json!([{"id": "i1", "type": "user_input_request", "requestId": "r1", "questions": [
                {"id": "q1", "header": "Deploy", "question": "Where to?", "multiSelect": true,
                 "options": [
                    {"label": "Staging", "description": "Safe"},
                    {"label": "Prod", "value": "production"},
                    {"label": "Prod again", "value": "production"}
                 ]}
            ]}]),
        );
        let found = describe("e/t1", &s, Value::Null).unwrap();
        assert_eq!(found["kind"], "question");
        assert_eq!(found["request"], "r1");
        assert_eq!(found["title"], "Ship it");
        let q = &found["questions"][0];
        assert_eq!(q["multiple"], true);
        assert_eq!(q["header"], "Deploy");
        assert_eq!(q["question"], "Where to?");
        let ids: Vec<&str> = q["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["Staging", "production"]);
        assert_eq!(q["options"][0]["subtitle"], "Safe");
    }

    #[test]
    fn an_approval_names_what_it_allows() {
        let s = state(
            json!([{"id": "r2", "status": "pending", "kind": "permission"}]),
            json!([{"id": "a1", "type": "approval_request", "requestId": "r2",
                    "prompt": "Use the network"}]),
        );
        let found = describe("e/t1", &s, Value::Null).unwrap();
        assert_eq!(found["kind"], "approval");
        assert_eq!(found["verb"], "Allow");
        assert_eq!(found["what"], "Use the network");
    }

    #[test]
    fn nothing_waiting_is_none() {
        let s = state(json!([]), json!([]));
        assert!(describe("e/t1", &s, Value::Null).is_none());
    }
}
