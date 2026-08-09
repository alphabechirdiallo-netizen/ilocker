// ============================================================
//  ilocClient.ts — couche de données
//
//  Ce module ne fait qu'une chose : appeler le binaire `iloc` en
//  sous-processus et typer son JSON. Aucune logique d'affichage ici.
//  Toutes les données viennent de `iloc studio <sous-commande>` —
//  jamais dupliquées ou recalculées côté extension, pour qu'il soit
//  structurellement impossible de diverger de ce que le binaire sait
//  réellement faire.
// ============================================================

import * as cp from 'child_process';
import * as vscode from 'vscode';

// ── Types miroir des structures Rust (commands/studio.rs) ──────

export interface ArgEntry {
  id: string;
  positional: boolean;
  required: boolean;
  help: string | null;
  takes_value: boolean;
  long_flag: string | null;
}

export type Danger = 'safe' | 'caution' | 'destructive';

export interface CommandDoc {
  summary: string;
  details: string;
  example: string;
  prerequisites: string[];
  danger: Danger;
  args: Record<string, { description: string }>;
}

export interface CommandEntry {
  path: string[];
  name: string;
  about: string | null;
  is_leaf: boolean;
  args: ArgEntry[];
  doc: CommandDoc | null;
}

export interface Snapshot {
  id: string;
  message: string;
  parent_id: string | null;
  created_at: string;
  file_count: number;
  total_bytes: number;
}

export interface GithubLink { owner: string; repo: string; linked_at: string; }
export interface VercelLink { project_id: string; team_id: string | null; linked_at: string; }
export interface SupabaseLink { project_ref: string; org_id: string; linked_at: string; }
export interface LastDeploy { git_sha: string | null; vercel_deployment_id: string | null; deployed_at: string; }

export interface DeployState {
  github: GithubLink | null;
  vercel: VercelLink | null;
  supabase: SupabaseLink | null;
  env_hashes: Record<string, string>;
  last_deploy: LastDeploy | null;
}

export interface ProjectStatus {
  initialised: boolean;
  vault_mode: string | null;
  sentinel_active: boolean;
  github_connected: boolean;
  vercel_connected: boolean;
  supabase_connected: boolean;
  snapshot_count: number;
}

// ── Résolution du binaire ───────────────────────────────────────

/** Trouve le binaire `iloc` : réglage explicite, sinon PATH. */
function ilocBinary(): string {
  const cfg = vscode.workspace.getConfiguration('ilockerStudio');
  const configured = cfg.get<string>('binaryPath');
  return configured && configured.trim().length > 0 ? configured : 'iloc';
}

function cwd(): string | undefined {
  return vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
}

/** Erreur typée : distingue "iloc absent" de "projet non initialisé"
 *  de "commande a échoué", pour que l'UI puisse réagir précisément
 *  plutôt qu'afficher un message générique. */
export class IlocError extends Error {
  constructor(public kind: 'not-found' | 'not-initialised' | 'failed', message: string) {
    super(message);
  }
}

function runIloc(args: string[]): Promise<string> {
  return new Promise((resolve, reject) => {
    cp.execFile(ilocBinary(), args, { cwd: cwd(), timeout: 15000, maxBuffer: 16 * 1024 * 1024 }, (err, stdout, stderr) => {
      if (err) {
        const code = (err as cp.ExecFileException).code;
        if (code === 'ENOENT') {
          reject(new IlocError('not-found', "Le binaire `iloc` est introuvable dans le PATH."));
        } else if (/not.*initiali[sz]ed|iloc init/i.test(stderr)) {
          reject(new IlocError('not-initialised', "Ce dossier n'est pas encore un projet ilocker."));
        } else {
          reject(new IlocError('failed', stderr.trim() || err.message));
        }
        return;
      }
      resolve(stdout);
    });
  });
}

export async function getManifest(): Promise<CommandEntry[]> {
  const out = await runIloc(['studio', 'manifest']);
  return JSON.parse(out);
}

export async function getSnapshots(limit?: number): Promise<Snapshot[]> {
  const args = ['studio', 'snapshots'];
  if (limit) { args.push('--limit', String(limit)); }
  const out = await runIloc(args);
  return JSON.parse(out);
}

export async function getDeployStatus(): Promise<DeployState> {
  const out = await runIloc(['studio', 'deploy-status']);
  return JSON.parse(out);
}

export async function getProjectStatus(): Promise<ProjectStatus> {
  const out = await runIloc(['studio', 'project-status']);
  return JSON.parse(out);
}

/** Compose et lance une commande dans un terminal intégré visible —
 *  jamais d'exécution cachée : l'utilisateur voit toujours la vraie
 *  commande et sa sortie réelle, comme s'il l'avait tapée lui-même. */
export function runInTerminal(commandLine: string): void {
  const terminal = vscode.window.terminals.find(t => t.name === 'ilocker Studio')
    ?? vscode.window.createTerminal('ilocker Studio');
  terminal.show();
  terminal.sendText(commandLine);
}
