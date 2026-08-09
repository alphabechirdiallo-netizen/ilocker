import * as vscode from 'vscode';
import { CommandCenterPanel } from './commandCenterPanel';
import { SidebarViewProvider } from './sidebarView';
import { ActivityLog } from './activityLog';

export function activate(context: vscode.ExtensionContext) {
  const activityLog = new ActivityLog(context);
  const sidebarProvider = new SidebarViewProvider(context.extensionUri, activityLog);

  context.subscriptions.push(
    vscode.window.registerWebviewViewProvider(SidebarViewProvider.viewType, sidebarProvider),

    vscode.commands.registerCommand('ilockerStudio.openCommandCenter', (tab?: string) => {
      CommandCenterPanel.createOrShow(context.extensionUri, activityLog, tab);
    }),

    vscode.commands.registerCommand('ilockerStudio.refresh', () => {
      sidebarProvider.refresh();
    }),
  );
}

export function deactivate() {}
