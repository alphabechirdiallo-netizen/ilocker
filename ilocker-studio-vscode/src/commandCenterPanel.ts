// ============================================================
//  commandCenterPanel.ts — le centre de commandes plein écran
//
//  Un seul webview panel avec 4 onglets internes (Commandes,
//  Snapshots, Déploiement, Activité) plutôt que 4 vues VS Code
//  séparées : un seul endroit où aller, une navigation cohérente.
// ============================================================

import * as vscode from 'vscode';
import * as iloc from './ilocClient';
import { ActivityLog } from './activityLog';
import { getWebviewHtml } from './webviewHtml';

export class CommandCenterPanel {
  public static current: CommandCenterPanel | undefined;
  private readonly panel: vscode.WebviewPanel;
  private disposables: vscode.Disposable[] = [];

  public static createOrShow(extensionUri: vscode.Uri, activityLog: ActivityLog, initialTab?: string) {
    const column = vscode.window.activeTextEditor?.viewColumn;

    if (CommandCenterPanel.current) {
      CommandCenterPanel.current.panel.reveal(column);
      if (initialTab) { CommandCenterPanel.current.postMessage({ type: 'selectTab', tab: initialTab }); }
      return;
    }

    const panel = vscode.window.createWebviewPanel(
      'ilockerStudioCommandCenter',
      'ilocker Studio',
      column ?? vscode.ViewColumn.One,
      { enableScripts: true, retainContextWhenHidden: true, localResourceRoots: [vscode.Uri.joinPath(extensionUri, 'media')] }
    );

    CommandCenterPanel.current = new CommandCenterPanel(panel, extensionUri, activityLog, initialTab);
  }

  private constructor(
    panel: vscode.WebviewPanel,
    private readonly extensionUri: vscode.Uri,
    private readonly activityLog: ActivityLog,
    initialTab?: string,
  ) {
    this.panel = panel;
    this.panel.webview.html = getWebviewHtml(this.panel.webview, this.extensionUri, initialTab);

    this.panel.webview.onDidReceiveMessage(msg => this.handleMessage(msg), null, this.disposables);
    this.panel.onDidDispose(() => this.dispose(), null, this.disposables);

    this.loadAll();
  }

  private postMessage(msg: unknown) {
    this.panel.webview.postMessage(msg);
  }

  private async loadAll() {
    // Chaque source est chargée indépendamment : si l'une échoue
    // (ex : projet non initialisé → pas de snapshots), les autres
    // s'affichent quand même. Jamais un seul échec ne bloque tout.
    this.loadManifest();
    this.loadSnapshots();
    this.loadDeployStatus();
    this.loadProjectStatus();
    this.postMessage({ type: 'activity', entries: this.activityLog.list() });
  }

  private async loadManifest() {
    try {
      const manifest = await iloc.getManifest();
      this.postMessage({ type: 'manifest', data: manifest });
    } catch (e) {
      this.postMessage({ type: 'manifestError', error: describeError(e) });
    }
  }

  private async loadSnapshots() {
    try {
      const snapshots = await iloc.getSnapshots(50);
      this.postMessage({ type: 'snapshots', data: snapshots });
    } catch (e) {
      this.postMessage({ type: 'snapshotsError', error: describeError(e) });
    }
  }

  private async loadDeployStatus() {
    try {
      const status = await iloc.getDeployStatus();
      this.postMessage({ type: 'deployStatus', data: status });
    } catch (e) {
      this.postMessage({ type: 'deployStatusError', error: describeError(e) });
    }
  }

  private async loadProjectStatus() {
    try {
      const status = await iloc.getProjectStatus();
      this.postMessage({ type: 'projectStatus', data: status });
    } catch (e) {
      this.postMessage({ type: 'projectStatusError', error: describeError(e) });
    }
  }

  private handleMessage(msg: any) {
    switch (msg.type) {
      case 'runCommand':
        iloc.runInTerminal(msg.commandLine);
        this.activityLog.record({ commandLine: msg.commandLine, path: msg.path, danger: msg.danger ?? 'safe' });
        this.postMessage({ type: 'activity', entries: this.activityLog.list() });
        return;
      case 'refresh':
        this.loadAll();
        return;
      case 'clearActivity':
        this.activityLog.clear();
        this.postMessage({ type: 'activity', entries: [] });
        return;
    }
  }

  private dispose() {
    CommandCenterPanel.current = undefined;
    this.panel.dispose();
    while (this.disposables.length) { this.disposables.pop()?.dispose(); }
  }
}

function describeError(e: unknown): { kind: string; message: string } {
  if (e instanceof iloc.IlocError) { return { kind: e.kind, message: e.message }; }
  return { kind: 'failed', message: e instanceof Error ? e.message : String(e) };
}
