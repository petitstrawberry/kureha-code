//! Revision-guarded, opt-in LSP type hints. Labels never enter the document.
use super::*;
use scarlet_ui::TextInlayHint;

#[derive(Default)]
pub(super) struct Requests {
    requested: BTreeMap<PathBuf, (DocumentId, u64, Instant, bool)>,
    pub(super) pending: BTreeMap<u64, (Snapshot, Instant)>,
    cached: BTreeMap<PathBuf, (DocumentId, u64, Vec<(usize, String)>)>,
}

impl Requests {
    pub(super) fn request(&mut self, client: &mut Client, doc: &Snapshot) {
        if doc.text.len() > kureha_code::syntax::MAX_BYTES
            || self.pending.values().any(|(old, _)| old.id == doc.id)
            || self
                .requested
                .get(&doc.path)
                .is_some_and(|(id, revision, time, complete)| {
                    *id == doc.id
                        && *revision == doc.revision
                        && (*complete || time.elapsed() < Duration::from_secs(2))
                })
        {
            return;
        }
        let params = json!({"textDocument":{"uri":uri(&doc.path)},"range":{
            "start":{"line":0,"character":0},"end":position(&doc.text, doc.text.len())}});
        if let Ok(id) = client.request("textDocument/inlayHint", params) {
            self.requested.insert(
                doc.path.clone(),
                (doc.id, doc.revision, Instant::now(), false),
            );
            self.pending.insert(id, (doc.clone(), Instant::now()));
        }
    }

    pub(super) fn refresh(&mut self, client: Option<&mut Client>) {
        self.requested.clear();
        if let Some(client) = client {
            for id in self.pending.keys() {
                let _ = client.notify("$/cancelRequest", json!({"id":id}));
            }
        }
        self.pending.clear();
    }

    pub(super) fn prune(
        &mut self,
        client: &mut Client,
        docs: &BTreeMap<PathBuf, OpenDocument>,
        enabled: bool,
    ) {
        self.cached.retain(|path, (id, rev, _)| {
            enabled
                && docs
                    .get(path)
                    .is_some_and(|d| d.id == *id && d.revision == *rev)
        });
        self.requested
            .retain(|path, _| enabled && docs.contains_key(path));
        self.pending.retain(|id, (doc, time)| {
            let keep = enabled
                && time.elapsed() < Duration::from_secs(15)
                && docs
                    .get(&doc.path)
                    .is_some_and(|d| d.id == doc.id && d.revision == doc.revision);
            if !keep {
                let _ = client.notify("$/cancelRequest", json!({"id":id}));
            }
            keep
        });
    }

    pub(super) fn response(
        &mut self,
        id: u64,
        result: &Value,
        error: bool,
        snapshots: &[Snapshot],
        enabled: bool,
    ) -> bool {
        let Some((doc, _)) = self.pending.remove(&id) else {
            return false;
        };
        if !enabled
            || error
            || !snapshots
                .iter()
                .any(|d| d.id == doc.id && d.path == doc.path && d.revision == doc.revision)
        {
            return false;
        }
        let hints = decode(&doc.text, result).unwrap_or_default();
        if let Some((_, _, _, complete)) = self.requested.get_mut(&doc.path) {
            *complete = true;
        }
        self.cached.insert(doc.path, (doc.id, doc.revision, hints));
        true
    }
}

impl App {
    pub(crate) fn language_inlay_hints(&self, id: DocumentId) -> Vec<TextInlayHint> {
        if !self.preferences.get().inline_type_hints {
            return Vec::new();
        }
        let color = self.theme.get().editor_colors().muted;
        let rt = self.runtime.lock().unwrap();
        let Some(doc) = rt.workspace.get(id) else {
            return Vec::new();
        };
        let Some(path) = doc.path.as_ref() else {
            return Vec::new();
        };
        rt.language
            .sessions
            .values()
            .filter(|session| session.ready && session.failure.is_none())
            .find_map(|session| session.inlays.cached.get(path))
            .filter(|(cached_id, revision, _)| {
                *cached_id == id && *revision == doc.editor.revision()
            })
            .map(|(_, _, hints)| {
                hints
                    .iter()
                    .map(|(byte, label)| TextInlayHint {
                        byte: *byte,
                        label: label.clone(),
                        color,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn decode(text: &str, result: &Value) -> Option<Vec<(usize, String)>> {
    if result.is_null() {
        return Some(Vec::new());
    }
    let items = result.as_array()?;
    if items.len() > 10_000 {
        return None;
    }
    let mut lines = vec![0];
    lines.extend(text.match_indices('\n').map(|(i, _)| i + 1));
    let mut hints = Vec::new();
    let mut total = 0;
    for item in items {
        // This option is specifically for types, not parameter names or chaining hints.
        if item.get("kind").and_then(Value::as_u64) != Some(1) {
            continue;
        }
        let pos = item.get("position")?;
        let start = *lines.get(usize::try_from(pos.get("line")?.as_u64()?).ok()?)?;
        let line = text[start..].split(['\r', '\n']).next()?;
        let byte = start + offset(line, &json!({"line":0,"character":pos.get("character")?}))?;
        let value = item.get("label")?;
        let mut label = if let Some(s) = value.as_str() {
            s.to_owned()
        } else {
            value
                .as_array()?
                .iter()
                .map(|p| p.get("value")?.as_str())
                .collect::<Option<String>>()?
        };
        if label.len() > 512 || label.chars().any(char::is_control) {
            return None;
        }
        if label.is_empty() {
            continue;
        }
        if item.get("paddingLeft").and_then(Value::as_bool) == Some(true) {
            label.insert(0, ' ');
        }
        if item.get("paddingRight").and_then(Value::as_bool) == Some(true) {
            label.push(' ');
        }
        total += label.len();
        if total > 256_000 {
            return None;
        }
        hints.push((byte, label));
    }
    hints.sort_by_key(|(byte, _)| *byte);
    hints.dedup();
    Some(hints)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_type_labels_and_utf16_without_parameter_hints() {
        let text = "let 日本 = \"😀\"; value";
        let result = json!([
            {"position":{"line":0,"character":6},"label":[{"value":": "},{"value":"&str"}],"kind":1},
            {"position":{"line":0,"character":14},"label":": u32","kind":1,"paddingRight":true},
            {"position":{"line":0,"character":0},"label":"arg:","kind":2}
        ]);
        let hints = decode(text, &result).unwrap();
        assert_eq!(hints, vec![(10, ": &str".into()), (20, ": u32 ".into())]);
        for position in [
            json!({"line":0,"character":11}),
            json!({"line":99,"character":0}),
        ] {
            assert!(decode(text, &json!([{"position":position,"label":": T","kind":1}])).is_none());
        }
        assert!(
            decode(
                text,
                &json!([{"position":{"line":0,"character":0},"label":"bad\nlabel","kind":1}])
            )
            .is_none()
        );
    }

    #[test]
    fn rejects_stale_disabled_and_closed_document_responses() {
        let doc = Snapshot {
            id: 7,
            path: PathBuf::from("/tmp/inlays.rs"),
            revision: 3,
            text: Arc::new("let a = 1;".into()),
            caret: 0,
            saved: true,
        };
        let data = json!([{"position":{"line":0,"character":5},"kind":1,"label":": i32"}]);
        let mut requests = Requests::default();
        let mut newer = doc.clone();
        newer.revision += 1;
        for (snapshots, enabled) in [
            (vec![newer], true),
            (vec![doc.clone()], false),
            (vec![], true),
        ] {
            requests.pending.insert(1, (doc.clone(), Instant::now()));
            assert!(!requests.response(1, &data, false, &snapshots, enabled));
            assert!(requests.cached.is_empty());
        }
        requests.pending.insert(2, (doc.clone(), Instant::now()));
        let mut moved = doc.clone();
        moved.caret = 8;
        assert!(requests.response(2, &data, false, &[moved], true));
        assert_eq!(requests.cached[&doc.path].2, vec![(5, ": i32".into())]);
        assert!(!requests.response(2, &data, false, &[doc], true));
    }

    #[test]
    fn preference_is_optional_and_validated() {
        assert!(!preferences::Preferences::default().inline_type_hints);
        assert!(
            preferences::Preferences::parse("inline_type_hints = true")
                .unwrap()
                .inline_type_hints
        );
        assert!(preferences::Preferences::parse("inline_type_hints = 'true'").is_err());
    }
}
