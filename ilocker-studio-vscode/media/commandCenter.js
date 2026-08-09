// ============================================================
//  commandCenter.js — logique du webview panel (vanilla JS,
//  aucune dépendance — un webview VS Code n'a pas de bundler,
//  et une UI de cette taille n'en a pas besoin).
// ============================================================
(function () {
  const vscode = acquireVsCodeApi();

  // ── État local ────────────────────────────────────────────────
  let manifest = [];
  let activeCategory = null;
  let searchQuery = '';
  let pendingCommand = null; // { entry, args: {} } — commande en attente de formulaire

  // ── Catégorisation (miroir exact de la génération STANDALONE.md) ──
  const CATEGORIES = [
    { id: 'core',      label: 'Snapshots locaux',      match: e => ['init','save','undo','log','status','dashboard'].includes(key(e)) },
    { id: 'vault',     label: 'Vault & sauvegarde 3-2-1', match: e => key(e).startsWith('vault.') },
    { id: 'cloud',     label: 'Cloud BYOC',             match: e => key(e).startsWith('cloud.') || key(e).startsWith('config.cloud.') || ['push','pull'].includes(key(e)) },
    { id: 'p2p',       label: 'Partage P2P',            match: e => ['share','clone','transfer-status'].includes(key(e)) },
    { id: 'hyperscale',label: 'Hyperscale',             match: e => key(e).startsWith('hyperscale.') },
    { id: 'github',    label: 'GitHub',                 match: e => key(e).startsWith('github.') || key(e) === 'connect' },
    { id: 'vercel',    label: 'Vercel',                 match: e => key(e).startsWith('vercel.') },
    { id: 'supabase',  label: 'Supabase',                match: e => key(e).startsWith('supabase.') },
    { id: 'deploy',    label: 'Déploiement orchestré',   match: e => key(e) === 'deploy' },
    { id: 'sentinel',  label: 'Sentinel',                match: e => key(e).startsWith('sentinel.') },
    { id: 'studio',    label: 'Studio',                  match: e => key(e).startsWith('studio.') },
    { id: 'account',   label: 'Réseau & compte',         match: e => ['login','logout','whoami','node.join','node.leave','node.start','node.status'].includes(key(e)) },
    { id: 'system',    label: 'Auto-gestion',            match: e => ['selfinstall','update','completion'].includes(key(e)) },
  ];

  function key(e) { return e.path.join('.'); }
  function categoryOf(e) { return CATEGORIES.find(c => c.match(e)) ?? { id: 'other', label: 'Autre' }; }

  // ── Construction de la ligne de commande depuis path + args ────
  function buildCommandLine(entry, values) {
    const parts = ['iloc', ...entry.path];
    for (const arg of entry.args) {
      const v = values[arg.id];
      if (arg.positional) {
        if (v !== undefined && v !== '') { parts.push(quoteIfNeeded(String(v))); }
      } else if (arg.takes_value) {
        if (v !== undefined && v !== '') { parts.push(arg.long_flag, quoteIfNeeded(String(v))); }
      } else {
        if (v === true) { parts.push(arg.long_flag); }
      }
    }
    return parts.join(' ');
  }
  function quoteIfNeeded(s) { return /\s/.test(s) ? '"' + s.replace(/"/g, '\\"') + '"' : s; }

  // ── Rendu : rail de catégories ──────────────────────────────────
  function renderCategories() {
    const rail = document.getElementById('categoriesRail');
    const counts = {};
    for (const e of manifest) { if (!e.is_leaf) continue; const c = categoryOf(e).id; counts[c] = (counts[c] || 0) + 1; }

    let html = `<div class="cat-item ${activeCategory === null ? 'active' : ''}" data-cat="">Toutes<span class="cat-count">${manifest.filter(e=>e.is_leaf).length}</span></div>`;
    for (const cat of CATEGORIES) {
      if (!counts[cat.id]) continue;
      html += `<div class="cat-item ${activeCategory === cat.id ? 'active' : ''}" data-cat="${cat.id}">${cat.label}<span class="cat-count">${counts[cat.id]}</span></div>`;
    }
    rail.innerHTML = html;
    rail.querySelectorAll('.cat-item').forEach(el => {
      el.addEventListener('click', () => { activeCategory = el.dataset.cat || null; renderCategories(); renderGrid(); });
    });
  }

  // ── Rendu : grille de commandes (groupée par catégorie si "Toutes") ──
  function renderGrid() {
    const grid = document.getElementById('cmdGrid');
    const q = searchQuery.trim().toLowerCase();

    let entries = manifest.filter(e => e.is_leaf);
    if (activeCategory) { entries = entries.filter(e => categoryOf(e).id === activeCategory); }
    if (q) {
      entries = entries.filter(e => {
        const hay = (key(e) + ' ' + (e.doc?.summary || '') + ' ' + (e.about || '')).toLowerCase();
        return hay.includes(q);
      });
    }

    if (entries.length === 0) {
      grid.innerHTML = `<div class="empty-state"><div class="big">🔍</div>Aucune commande ne correspond à « ${escapeHtml(searchQuery)} ».</div>`;
      return;
    }

    if (activeCategory || q) {
      grid.innerHTML = entries.map(cardHtml).join('');
    } else {
      // Vue "Toutes" : groupée par catégorie, dans l'ordre défini
      let html = '';
      for (const cat of CATEGORIES) {
        const inCat = entries.filter(e => categoryOf(e).id === cat.id);
        if (inCat.length === 0) continue;
        html += `<div class="cat-heading">${cat.label}</div><div class="cmd-grid">${inCat.map(cardHtml).join('')}</div>`;
      }
      grid.innerHTML = html;
    }

    grid.querySelectorAll('.cmd-card').forEach(el => {
      el.addEventListener('click', () => onCardClick(entries.find(e => key(e) === el.dataset.key)));
    });
  }

  function cardHtml(e) {
    const doc = e.doc;
    const sig = 'iloc ' + e.path.join(' ') + (e.args.length ? ' …' : '');
    const badge = doc && doc.danger !== 'safe' ? `<span class="badge ${doc.danger}">${doc.danger === 'destructive' ? '🔴' : '⚠️'}</span>` : '';
    return `<div class="cmd-card" data-key="${key(e)}">
      <div class="cmd-card-head"><span class="cmd-sig">${escapeHtml(sig)}</span>${badge}</div>
      <div class="cmd-summary">${escapeHtml(doc?.summary || e.about || '')}</div>
    </div>`;
  }

  // ── Clic sur une carte : exécution directe, ou formulaire ──────
  function onCardClick(entry) {
    if (!entry) return;
    const needsForm = entry.args.some(a => a.positional && a.required) || entry.args.length > 0;
    if (!needsForm) {
      confirmAndRun(entry, {});
      return;
    }
    openForm(entry);
  }

  function openForm(entry) {
    pendingCommand = { entry };
    const doc = entry.doc;
    const backdrop = document.getElementById('modalBackdrop');
    const body = document.getElementById('modalBody');

    const dangerWarning = doc && doc.danger !== 'safe'
      ? `<div class="danger-warning">${doc.danger === 'destructive' ? '🔴 Action difficilement réversible.' : '⚠️ Cette action modifie un état externe.'} ${escapeHtml(doc.details || '')}</div>`
      : '';

    const fields = entry.args.map(a => {
      const help = doc?.args?.[a.id]?.description || a.help || '';
      const label = a.id.replace(/_/g, ' ');
      if (!a.takes_value) {
        return `<div class="field checkbox-row"><input type="checkbox" id="f_${a.id}"><label for="f_${a.id}" style="margin:0">${escapeHtml(a.long_flag || label)}</label></div>
                ${help ? `<div class="field-help">${escapeHtml(help)}</div>` : ''}`;
      }
      return `<div class="field">
        <label for="f_${a.id}">${escapeHtml(label)}${a.required ? ' *' : ''}</label>
        <input type="text" id="f_${a.id}" placeholder="${a.required ? 'requis' : 'optionnel'}">
        ${help ? `<div class="field-help">${escapeHtml(help)}</div>` : ''}
      </div>`;
    }).join('');

    body.innerHTML = `
      <h3>iloc ${entry.path.join(' ')}</h3>
      <div class="modal-sig">${escapeHtml(doc?.summary || '')}</div>
      ${dangerWarning}
      ${fields}
      <div class="modal-actions">
        <button id="modalCancel">Annuler</button>
        <button class="primary" id="modalRun">Lancer</button>
      </div>`;
    backdrop.classList.add('open');

    document.getElementById('modalCancel').onclick = closeForm;
    document.getElementById('modalRun').onclick = () => {
      const values = {};
      for (const a of entry.args) {
        const el = document.getElementById('f_' + a.id);
        values[a.id] = a.takes_value ? el.value : el.checked;
      }
      closeForm();
      confirmAndRun(entry, values);
    };
  }
  function closeForm() { document.getElementById('modalBackdrop').classList.remove('open'); pendingCommand = null; }

  function confirmAndRun(entry, values) {
    const line = buildCommandLine(entry, values);
    vscode.postMessage({ type: 'runCommand', commandLine: line, path: entry.path, danger: entry.doc?.danger || 'safe' });
  }

  // ── Onglet Snapshots ─────────────────────────────────────────
  function renderSnapshots(list) {
    const el = document.getElementById('snapshotsContent');
    if (!list || list.length === 0) {
      el.innerHTML = `<div class="empty-state"><div class="big">📸</div>Aucun snapshot pour l'instant.<br><br>Lancez <code>iloc save "message"</code> pour créer le premier.</div>`;
      return;
    }
    el.innerHTML = '<div class="timeline">' + list.map((s, i) => `
      <div class="snap-item">
        <div class="snap-dot"></div>
        <div class="snap-msg">${escapeHtml(s.message)}${i === 0 ? '<span class="snap-badge-latest">dernier</span>' : ''}</div>
        <div class="snap-meta">${formatDate(s.created_at)} · ${s.file_count} fichier(s) · ${humanBytes(s.total_bytes)}</div>
      </div>`).join('') + '</div>';
  }

  // ── Onglet Déploiement ───────────────────────────────────────
  function renderDeploy(state) {
    const el = document.getElementById('deployContent');
    if (!state) { el.innerHTML = emptyDeploy(); return; }

    function card(name, dotClass, link, detailHtml) {
      return `<div class="card provider-card">
        <div class="provider-name"><span class="pulse-dot ${dotClass}"></span> ${name}</div>
        ${link ? detailHtml : `<div class="provider-detail">Non lié — <code>iloc connect ${name.toLowerCase()}</code></div>`}
      </div>`;
    }

    let html = '<div class="provider-grid">';
    html += card('GitHub', state.github ? 'amber' : 'off', state.github, state.github ? `<div class="provider-detail"><span class="k">repo</span> ${escapeHtml(state.github.owner)}/${escapeHtml(state.github.repo)}</div>` : '');
    html += card('Vercel', state.vercel ? 'teal' : 'off', state.vercel, state.vercel ? `<div class="provider-detail"><span class="k">projet</span> ${escapeHtml(state.vercel.project_id)}</div>` : '');
    html += card('Supabase', state.supabase ? 'teal' : 'off', state.supabase, state.supabase ? `<div class="provider-detail"><span class="k">projet</span> ${escapeHtml(state.supabase.project_ref)}</div>` : '');
    html += '</div>';

    if (state.last_deploy) {
      html += `<hr><div class="card" style="max-width:400px"><div class="provider-detail"><span class="k">dernier déploiement</span></div>
        <div style="margin-top:6px">${formatDate(state.last_deploy.deployed_at)}</div>
        ${state.last_deploy.git_sha ? `<div class="provider-detail mono">${escapeHtml(state.last_deploy.git_sha.slice(0,10))}</div>` : ''}</div>`;
    }
    el.innerHTML = html || emptyDeploy();
  }
  function emptyDeploy() {
    return `<div class="empty-state"><div class="big">🚀</div>Aucun provider lié pour l'instant.<br><br>Lancez <code>iloc deploy</code> pour connecter GitHub, Vercel et Supabase automatiquement.</div>`;
  }

  // ── Onglet Activité ──────────────────────────────────────────
  function renderActivity(entries) {
    const el = document.getElementById('activityContent');
    if (!entries || entries.length === 0) {
      el.innerHTML = `<div class="empty-state"><div class="big">🕓</div>Aucune commande lancée depuis ce centre de commandes pour l'instant.</div>`;
      return;
    }
    el.innerHTML = `<button id="clearActivityBtn" style="margin-bottom:12px">Effacer l'historique</button>` +
      entries.map(a => `<div class="activity-row"><div class="activity-time">${formatDate(a.timestamp)}</div><div class="activity-cmd">${escapeHtml(a.commandLine)}</div></div>`).join('');
    document.getElementById('clearActivityBtn').onclick = () => vscode.postMessage({ type: 'clearActivity' });
  }

  // ── Barre de statut projet ───────────────────────────────────
  function renderStatusChip(status) {
    const chip = document.getElementById('projectStatusChip');
    const pulse = document.getElementById('masterPulse');
    if (!status || !status.initialised) {
      chip.textContent = 'projet non initialisé';
      pulse.className = 'pulse-dot off';
      return;
    }
    const connected = [status.github_connected && 'GitHub', status.vercel_connected && 'Vercel', status.supabase_connected && 'Supabase'].filter(Boolean);
    chip.textContent = `${status.snapshot_count} snapshot(s) · ${connected.length ? connected.join(', ') : 'aucun provider'}${status.sentinel_active ? ' · Sentinel actif' : ''}`;
    pulse.className = 'pulse-dot' + (status.sentinel_active ? '' : ' amber');
  }

  // ── Utilitaires ──────────────────────────────────────────────
  function escapeHtml(s) { return String(s ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c])); }
  function formatDate(iso) { try { return new Date(iso).toLocaleString('fr-FR', { day:'2-digit', month:'short', hour:'2-digit', minute:'2-digit' }); } catch { return iso; } }
  function humanBytes(n) { if (n < 1024) return n + ' o'; if (n < 1048576) return (n/1024).toFixed(1) + ' Ko'; return (n/1048576).toFixed(1) + ' Mo'; }

  // ── Onglets ──────────────────────────────────────────────────
  function selectTab(tab) {
    document.querySelectorAll('.tab-btn').forEach(b => b.classList.toggle('active', b.dataset.tab === tab));
    document.querySelectorAll('.tab-content').forEach(c => c.classList.toggle('active', c.id === 'tab-' + tab));
  }
  document.querySelectorAll('.tab-btn').forEach(b => b.addEventListener('click', () => selectTab(b.dataset.tab)));
  selectTab(window.__ILK_INITIAL_TAB__ || 'commands');

  document.getElementById('searchInput').addEventListener('input', e => { searchQuery = e.target.value; renderGrid(); });
  document.getElementById('modalBackdrop').addEventListener('click', e => { if (e.target.id === 'modalBackdrop') closeForm(); });

  // ── Réception des données depuis l'extension ────────────────
  window.addEventListener('message', event => {
    const msg = event.data;
    switch (msg.type) {
      case 'manifest': manifest = msg.data; renderCategories(); renderGrid(); break;
      case 'manifestError': document.getElementById('cmdGrid').innerHTML = errorHtml(msg.error); break;
      case 'snapshots': renderSnapshots(msg.data); break;
      case 'snapshotsError': document.getElementById('snapshotsContent').innerHTML = errorHtml(msg.error); break;
      case 'deployStatus': renderDeploy(msg.data); break;
      case 'deployStatusError': document.getElementById('deployContent').innerHTML = errorHtml(msg.error); break;
      case 'projectStatus': renderStatusChip(msg.data); break;
      case 'activity': renderActivity(msg.entries); break;
      case 'selectTab': selectTab(msg.tab); break;
    }
  });

  function errorHtml(err) {
    if (err.kind === 'not-initialised') {
      return `<div class="empty-state"><div class="big">📁</div>Ce dossier n'est pas encore un projet ilocker.<br><br><code>iloc init</code> pour commencer.</div>`;
    }
    if (err.kind === 'not-found') {
      return `<div class="empty-state"><div class="big">⚠️</div>Le binaire <code>iloc</code> est introuvable.<br><br>Vérifiez qu'il est installé et dans le PATH.</div>`;
    }
    return `<div class="empty-state"><div class="big">⚠️</div>${escapeHtml(err.message)}</div>`;
  }
})();
