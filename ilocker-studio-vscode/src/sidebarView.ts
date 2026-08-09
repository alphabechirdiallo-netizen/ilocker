// ============================================================
//  sidebarView.ts — l'assistant continu docké dans la barre
//  latérale. Rétractable nativement par VS Code (clic sur l'icône,
//  ou glisser pour redimensionner) — aucune logique de repli à
//  gérer nous-mêmes, c'est le rôle natif d'une Webview View.
// ============================================================

import * as vscode from 'vscode';
import * as iloc from './ilocClient';
import { ActivityLog } from './activityLog';

export class SidebarViewProvider implements vscode.WebviewViewProvider {
  public static readonly viewType = 'ilockerStudio.sidebar';
  private view?: vscode.WebviewView;

  constructor(
    private readonly extensionUri: vscode.Uri,
    private readonly activityLog: ActivityLog,
  ) {}

  resolveWebviewView(webviewView: vscode.WebviewView) {
    this.view = webviewView;
    webviewView.webview.options = { enableScripts: true, localResourceRoots: [vscode.Uri.joinPath(this.extensionUri, 'media')] };
    webviewView.webview.html = this.html(webviewView.webview);

    webviewView.webview.onDidReceiveMessage(msg => {
      switch (msg.type) {
        case 'openCommandCenter':
          vscode.commands.executeCommand('ilockerStudio.openCommandCenter', msg.tab);
          return;
        case 'runCommand':
          iloc.runInTerminal(msg.commandLine);
          this.activityLog.record({ commandLine: msg.commandLine, danger: 'safe' });
          return;
        case 'refresh':
          this.refresh();
          return;
      }
    });

    webviewView.onDidChangeVisibility(() => { if (webviewView.visible) { this.refresh(); } });
    this.refresh();
  }

  public async refresh() {
    if (!this.view) return;
    try {
      const status = await iloc.getProjectStatus();
      this.view.webview.postMessage({ type: 'projectStatus', data: status });
    } catch (e) {
      this.view.webview.postMessage({ type: 'projectStatusError', error: e instanceof iloc.IlocError ? e.kind : 'failed' });
    }
    try {
      const snapshots = await iloc.getSnapshots(3);
      this.view.webview.postMessage({ type: 'recentSnapshots', data: snapshots });
    } catch { /* silencieux — la carte statut suffit à expliquer pourquoi */ }
  }

  private html(webview: vscode.Webview): string {
    const styleUri = webview.asWebviewUri(vscode.Uri.joinPath(this.extensionUri, 'media', 'styles.css'));
    const scriptUri = webview.asWebviewUri(vscode.Uri.joinPath(this.extensionUri, 'media', 'sidebar.js'));
    const nonce = String(Date.now());
    return /* html */ `<!DOCTYPE html>
<html lang="fr"><head><meta charset="UTF-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src ${webview.cspSource}; script-src 'nonce-${nonce}';">
<link href="${styleUri}" rel="stylesheet">
<style>
  body { padding: 10px 12px; }
  .status-card { display: flex; flex-direction: column; gap: 6px; margin-bottom: 14px; }
  .status-row { display: flex; align-items: center; gap: 7px; font-size: 12px; }
  .status-row .k { color: var(--ilk-text-dim); }
  h4 { font-family: var(--ilk-mono); font-size: 10.5px; text-transform: uppercase; letter-spacing: .06em; color: var(--ilk-text-faint); margin: 16px 0 8px; }
  .quick-btn { display: block; width: 100%; text-align: left; margin-bottom: 6px; }
  .mini-snap { font-size: 11.5px; padding: 5px 0; border-bottom: 1px solid var(--ilk-border); }
  .mini-snap .msg { color: var(--ilk-text); }
  .mini-snap .meta { color: var(--ilk-text-faint); font-family: var(--ilk-mono); font-size: 10px; }
  .open-btn { width: 100%; margin-top: 14px; }
</style>
</head><body>
  <div id="statusCard" class="status-card">…</div>
  <div id="quickActions"></div>
  <h4 id="recentHeading" style="display:none">Derniers snapshots</h4>
  <div id="recentSnaps"></div>
  <button class="primary open-btn" id="openBtn">Ouvrir le centre de commandes</button>
<script nonce="${nonce}">window.__ILK_INITIAL__ = true;</script>
<script nonce="${nonce}" src="${scriptUri}"></script>
</body></html>`;
  }
}
