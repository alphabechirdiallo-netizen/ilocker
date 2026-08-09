import * as vscode from 'vscode';

export function getWebviewHtml(webview: vscode.Webview, extensionUri: vscode.Uri, initialTab?: string): string {
  const styleUri = webview.asWebviewUri(vscode.Uri.joinPath(extensionUri, 'media', 'styles.css'));
  const scriptUri = webview.asWebviewUri(vscode.Uri.joinPath(extensionUri, 'media', 'commandCenter.js'));
  const nonce = String(Date.now()) + Math.random().toString(36).slice(2);

  return /* html */ `<!DOCTYPE html>
<html lang="fr">
<head>
<meta charset="UTF-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src ${webview.cspSource}; script-src 'nonce-${nonce}'; font-src ${webview.cspSource};">
<link href="${styleUri}" rel="stylesheet">
<title>ilocker Studio</title>
<style>
  html, body { height: 100%; }
  .app { display: flex; flex-direction: column; height: 100vh; }

  .topbar {
    display: flex; align-items: center; gap: 18px;
    padding: 10px 16px; border-bottom: 1px solid var(--ilk-border);
    background: var(--ilk-surface);
  }
  .brand { font-family: var(--ilk-mono); font-weight: 700; font-size: 13px; letter-spacing: 0.02em; display: flex; align-items: center; gap: 8px; }
  .tabs { display: flex; gap: 2px; margin-left: 8px; }
  .tab-btn {
    background: transparent; border: 1px solid transparent; color: var(--ilk-text-dim);
    padding: 6px 12px; border-radius: var(--ilk-radius-sm); font-size: 12.5px; cursor: pointer;
  }
  .tab-btn:hover { color: var(--ilk-text); }
  .tab-btn.active { background: var(--ilk-surface-alt); color: var(--ilk-text); border-color: var(--ilk-border); }
  .topbar-spacer { flex: 1; }
  .status-chip { display: flex; align-items: center; gap: 6px; font-size: 11.5px; color: var(--ilk-text-dim); font-family: var(--ilk-mono); }

  .tab-content { flex: 1; overflow: hidden; display: none; }
  .tab-content.active { display: flex; flex-direction: column; }

  /* ── Onglet Commandes ─────────────────────────────────────── */
  #tab-commands { flex-direction: row; }
  .categories-rail {
    width: 220px; flex-shrink: 0; overflow-y: auto; border-right: 1px solid var(--ilk-border);
    padding: 10px 8px;
  }
  .cat-item {
    padding: 7px 10px; border-radius: var(--ilk-radius-sm); font-size: 12.5px; cursor: pointer;
    color: var(--ilk-text-dim); display: flex; justify-content: space-between; align-items: center;
  }
  .cat-item:hover { background: var(--ilk-surface); color: var(--ilk-text); }
  .cat-item.active { background: var(--ilk-surface-alt); color: var(--ilk-text); font-weight: 600; }
  .cat-count { font-family: var(--ilk-mono); font-size: 10.5px; color: var(--ilk-text-faint); }

  .commands-main { flex: 1; overflow-y: auto; padding: 14px 18px; }
  .search-row { margin-bottom: 14px; max-width: 480px; }
  .cmd-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(300px, 1fr)); gap: 10px; }
  .cmd-card {
    background: var(--ilk-surface); border: 1px solid var(--ilk-border); border-radius: var(--ilk-radius);
    padding: 12px 13px; cursor: pointer; transition: border-color .12s ease;
  }
  .cmd-card:hover { border-color: var(--ilk-teal-dim); }
  .cmd-card-head { display: flex; justify-content: space-between; align-items: baseline; gap: 8px; margin-bottom: 5px; }
  .cmd-sig { font-size: 12px; color: var(--ilk-amber); font-weight: 600; word-break: break-word; }
  .cmd-summary { color: var(--ilk-text-dim); font-size: 12px; line-height: 1.45; }
  .cat-heading { font-family: var(--ilk-mono); font-size: 11px; text-transform: uppercase; letter-spacing: 0.06em; color: var(--ilk-text-faint); margin: 18px 0 8px; }
  .cat-heading:first-child { margin-top: 0; }

  /* ── Onglet Snapshots ─────────────────────────────────────── */
  #tab-snapshots { padding: 18px 24px; overflow-y: auto; }
  .timeline { position: relative; max-width: 640px; margin: 0 auto; padding-left: 22px; }
  .timeline::before { content: ''; position: absolute; left: 5px; top: 6px; bottom: 6px; width: 1px; background: var(--ilk-border); }
  .snap-item { position: relative; padding-bottom: 18px; }
  .snap-dot { position: absolute; left: -22px; top: 4px; width: 11px; height: 11px; border-radius: 50%; background: var(--ilk-teal); border: 2px solid var(--ilk-bg); }
  .snap-item:first-child .snap-dot { background: var(--ilk-amber); }
  .snap-msg { font-weight: 600; font-size: 13px; }
  .snap-meta { color: var(--ilk-text-dim); font-size: 11.5px; margin-top: 3px; font-family: var(--ilk-mono); }
  .snap-badge-latest { font-size: 10px; font-family: var(--ilk-mono); color: var(--ilk-amber); border: 1px solid var(--ilk-amber-dim); padding: 1px 6px; border-radius: 3px; margin-left: 8px; }

  /* ── Onglet Déploiement ───────────────────────────────────── */
  #tab-deploy { padding: 18px 24px; overflow-y: auto; }
  .provider-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); gap: 12px; max-width: 760px; }
  .provider-card { display: flex; flex-direction: column; gap: 8px; }
  .provider-name { font-weight: 700; font-family: var(--ilk-mono); font-size: 13px; display: flex; align-items: center; gap: 8px; }
  .provider-detail { font-size: 12px; color: var(--ilk-text-dim); }
  .provider-detail .k { color: var(--ilk-text-faint); }

  /* ── Onglet Activité ──────────────────────────────────────── */
  #tab-activity { padding: 18px 24px; overflow-y: auto; }
  .activity-row { display: flex; gap: 12px; padding: 8px 0; border-bottom: 1px solid var(--ilk-border); align-items: baseline; }
  .activity-time { font-family: var(--ilk-mono); font-size: 11px; color: var(--ilk-text-faint); width: 130px; flex-shrink: 0; }
  .activity-cmd { font-family: var(--ilk-mono); font-size: 12px; }

  .empty-state { text-align: center; color: var(--ilk-text-dim); padding: 48px 20px; max-width: 420px; margin: 0 auto; }
  .empty-state .big { font-size: 28px; margin-bottom: 10px; }
  .empty-state code { background: var(--ilk-surface-alt); padding: 2px 6px; border-radius: 3px; }

  /* ── Modale formulaire d'arguments ────────────────────────── */
  .modal-backdrop { position: fixed; inset: 0; background: rgba(0,0,0,.55); display: none; align-items: center; justify-content: center; z-index: 50; }
  .modal-backdrop.open { display: flex; }
  .modal { background: var(--ilk-surface); border: 1px solid var(--ilk-border); border-radius: var(--ilk-radius); width: 460px; max-width: 92vw; max-height: 84vh; overflow-y: auto; padding: 18px 20px; }
  .modal h3 { margin: 0 0 4px; font-family: var(--ilk-mono); font-size: 14px; }
  .modal .modal-sig { font-family: var(--ilk-mono); font-size: 11.5px; color: var(--ilk-text-dim); margin-bottom: 12px; }
  .modal .field { margin-bottom: 12px; }
  .modal label { display: block; font-size: 12px; margin-bottom: 4px; color: var(--ilk-text-dim); }
  .modal .field-help { font-size: 11px; color: var(--ilk-text-faint); margin-top: 3px; }
  .modal .checkbox-row { display: flex; align-items: center; gap: 7px; }
  .modal-actions { display: flex; justify-content: flex-end; gap: 8px; margin-top: 16px; }
  .danger-warning { background: var(--ilk-red-dim); color: #FFD4D4; padding: 8px 10px; border-radius: var(--ilk-radius-sm); font-size: 12px; margin-bottom: 12px; }
</style>
</head>
<body>
<div class="app">
  <div class="topbar">
    <div class="brand"><span class="pulse-dot" id="masterPulse"></span> ilocker studio</div>
    <div class="tabs">
      <button class="tab-btn active" data-tab="commands">Commandes</button>
      <button class="tab-btn" data-tab="snapshots">Snapshots</button>
      <button class="tab-btn" data-tab="deploy">Déploiement</button>
      <button class="tab-btn" data-tab="activity">Activité</button>
    </div>
    <div class="topbar-spacer"></div>
    <div class="status-chip" id="projectStatusChip">…</div>
  </div>

  <div id="tab-commands" class="tab-content active">
    <div class="categories-rail" id="categoriesRail"></div>
    <div class="commands-main">
      <div class="search-row"><input type="search" id="searchInput" placeholder="Chercher une commande… (nom, mot-clé)"></div>
      <div id="cmdGrid" class="cmd-grid"></div>
    </div>
  </div>

  <div id="tab-snapshots" class="tab-content"><div id="snapshotsContent"></div></div>
  <div id="tab-deploy" class="tab-content"><div id="deployContent"></div></div>
  <div id="tab-activity" class="tab-content"><div id="activityContent"></div></div>
</div>

<div class="modal-backdrop" id="modalBackdrop">
  <div class="modal" id="modalBody"></div>
</div>

<script nonce="${nonce}">window.__ILK_INITIAL_TAB__ = ${JSON.stringify(initialTab ?? 'commands')};</script>
<script nonce="${nonce}" src="${scriptUri}"></script>
</body>
</html>`;
}
