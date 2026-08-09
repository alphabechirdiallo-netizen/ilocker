// ============================================================
//  activityLog.ts — historique des commandes lancées via l'extension
//
//  Ne concerne QUE les commandes exécutées depuis l'UI de l'extension
//  (clic dans le centre de commandes) — pas un historique shell
//  général, qu'aucune API ne pourrait fournir de façon fiable.
//  Persisté dans le workspaceState de VS Code : propre à ce dossier
//  de projet, survit à la fermeture/réouverture de l'éditeur.
// ============================================================

import * as vscode from 'vscode';

export interface ActivityEntry {
  commandLine: string;
  /** Chemin de la commande dans le catalogue, ex: ["github","repo","create"] — absent pour une entrée pré-extension */
  path?: string[];
  timestamp: string;
  danger: 'safe' | 'caution' | 'destructive';
}

const STORAGE_KEY = 'ilockerStudio.activityLog';
const MAX_ENTRIES = 100;

export class ActivityLog {
  constructor(private context: vscode.ExtensionContext) {}

  list(): ActivityEntry[] {
    return this.context.workspaceState.get<ActivityEntry[]>(STORAGE_KEY, []);
  }

  record(entry: Omit<ActivityEntry, 'timestamp'>): void {
    const entries = this.list();
    entries.unshift({ ...entry, timestamp: new Date().toISOString() });
    if (entries.length > MAX_ENTRIES) { entries.length = MAX_ENTRIES; }
    this.context.workspaceState.update(STORAGE_KEY, entries);
  }

  clear(): void {
    this.context.workspaceState.update(STORAGE_KEY, []);
  }
}
