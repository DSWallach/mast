//! Interactive controls for the decision-sheet frame.
use mast_proto::decision_sheet::{SheetDecision, SheetDecisionDrafts};

pub(crate) fn with_state(
    source: &str,
    drafts: Option<&SheetDecisionDrafts>,
    sent: &[SheetDecision],
) -> String {
    let state = serde_json::json!({"drafts": drafts.map(|d| &d.decisions), "sent": sent})
        .to_string()
        .replace('<', "\\u003c")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");
    format!("{source}<style>{CSS}</style><script>window.__mastSheetState={state};{SCRIPT}</script>")
}

const CSS: &str = r#"
.mk-sheet-actions { display:flex; flex-wrap:wrap; align-items:center; gap:6px; margin-top:8px }
.mk-sheet-actions[hidden] { display:none }
.mk-sheet-actions button,.mk-sheet-actions input,.mk-sheet-bar button { font:inherit; color:inherit; border:1px solid var(--border); border-radius:5px; background:var(--background); padding:4px 8px }
.mk-sheet-actions button,.mk-sheet-bar button { cursor:pointer }
.mk-sheet-actions input { max-width:180px }
.mk-sheet-bar { position:sticky; bottom:0; display:flex; align-items:center; gap:10px; padding:10px; background:var(--background); border:1px solid var(--border); border-radius:6px; z-index:2 }
.mk-sheet-bar[hidden] { display:none }
"#;

const SCRIPT: &str = r#"(() => {
  const initial = window.__mastSheetState || {};
  const drafts = new Map((initial.drafts || []).map(d => [d.rowId,d]));
  const sent = new Map((initial.sent || []).map(d => [d.rowId,d]));
  const post = o => { try { parent.postMessage({mastMsg:o}, '*'); } catch (_) {} };
  const bar = document.createElement('div'); bar.className = 'mk-sheet-bar';
  const label = document.createElement('span');
  const send = document.createElement('button'); send.type='button'; send.textContent='Send';
  const discard = document.createElement('button'); discard.type='button'; discard.textContent='Discard';
  bar.append(label, send, discard); document.querySelector('.mk-sheet').appendChild(bar);
  const update = () => {
    bar.hidden = drafts.size === 0; label.textContent = `${drafts.size} answers ready`;
    document.querySelectorAll('[data-sheet-row]').forEach(row => {
      const id = row.getAttribute('data-sheet-row'); const controls = row.querySelector('.mk-sheet-actions');
      if (!controls) return;
      const saved = sent.get(id), draft = drafts.get(id);
      controls.hidden = !!saved;
      const chip = row.querySelector('.mk-chip');
      if (saved && chip) chip.textContent = saved.outcome === 'decided' ? 'Accepted' : 'Rejected';
      const status = controls.querySelector('[data-sheet-status]');
      status.textContent = draft ? (draft.outcome === 'decided' ? 'Accepted (not sent)' : 'Rejected (not sent)') : '';
      controls.querySelector('[data-sheet-undo]').hidden = !draft;
      const input = controls.querySelector('[data-sheet-note]');
      if (document.activeElement !== input) input.value = draft?.note || '';
    });
  };
  document.querySelectorAll('[data-sheet-row]').forEach(row => {
    const id = row.getAttribute('data-sheet-row'), controls = row.querySelector('.mk-sheet-actions');
    if (!controls || sent.has(id)) return;
    const input = controls.querySelector('[data-sheet-note]');
    const stage = outcome => { const note=input.value; drafts.set(id,{rowId:id,outcome,note}); post({t:'sheet-stage',id,outcome,note}); update(); };
    controls.querySelector('[data-sheet-accept]').onclick = () => stage('decided');
    controls.querySelector('[data-sheet-reject]').onclick = () => stage('rejected');
    controls.querySelector('[data-sheet-undo]').onclick = () => { drafts.delete(id); post({t:'sheet-undo',id}); update(); };
    input.oninput = () => { const draft = drafts.get(id); if (draft) stage(draft.outcome); };
  });
  discard.onclick = () => { drafts.clear(); post({t:'sheet-discard'}); update(); };
  const setBusy = busy => { send.disabled=busy; discard.disabled=busy; document.querySelectorAll('.mk-sheet-actions button,.mk-sheet-actions input').forEach(node => node.disabled=busy); };
  send.onclick = () => { setBusy(true); post({t:'sheet-send'}); };
  window.addEventListener('message', e => { if (!e.data) return; if (e.data.mastSheetEnableSend) setBusy(false); if (e.data.mastSheetSent) { e.data.mastSheetSent.forEach(d => {sent.set(d.rowId,d); drafts.delete(d.rowId);}); setBusy(false); update(); } });
  update();
})();"#;
