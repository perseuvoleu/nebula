'use strict';
const $ = id => document.getElementById(id);
let token = sessionStorage.getItem('nebula-token') || '';
let selected = null, generation = 0, timer, polling = false, busy = false;
const drafts = new Map();
function notice(text) { $('notice').textContent = text; }
async function api(path, body, query = {}) {
  const response = await fetch('/api/' + path + '?' + new URLSearchParams(query), {
    method: body ? 'POST' : 'GET', cache: 'no-store',
    headers: {Authorization: 'Bearer ' + token, ...(body ? {'Content-Type': 'application/json'} : {})},
    ...(body ? {body: JSON.stringify(body)} : {}), signal: AbortSignal.timeout(30000)
  });
  const data = await response.json();
  if (response.status === 401) logout();
  if (!response.ok) throw new Error(data.error || 'Cererea a eșuat');
  return data;
}
function controls() {
  $('message').disabled = !selected || busy;
  $('send').disabled = !selected || busy;
  $('project').disabled = busy;
  for (const input of $('new-form').querySelectorAll('button, textarea, select')) input.disabled = busy;
}
function logout() {
  token = ''; sessionStorage.removeItem('nebula-token'); generation++;
  clearTimeout(timer); selected = null; drafts.clear(); $('message').value = '';
  $('prompt').value = ''; $('output').textContent = ''; $('sessions').replaceChildren();
  $('action-status').textContent = '';
  $('workspace').hidden = true; $('login').hidden = false; $('logout').hidden = true;
  notice('Deconectat. Agenții continuă pe server.');
}
function option(value, text) { const o = document.createElement('option'); o.value = value; o.textContent = text; return o; }
async function connect() {
  const data = await api('projects');
  sessionStorage.setItem('nebula-token', token); $('token').value = '';
  $('project').replaceChildren(...data.projects.map(p => option(p, p)));
  $('login').hidden = true; $('workspace').hidden = false; $('logout').hidden = false;
  changeProject();
}
function saveDraft() { if (selected) drafts.set($('project').value + ':' + selected.id, $('message').value); }
function selectSession(row) {
  if (busy) return;
  saveDraft(); selected = row; generation++;
  $('session-title').textContent = row.name;
  $('message').value = drafts.get($('project').value + ':' + row.id) || '';
  $('output').textContent = 'Se încarcă…'; controls(); schedule(0);
}
function changeProject() {
  generation++; selected = null; $('message').value = ''; $('output').textContent = '';
  $('session-title').textContent = 'Alege o sesiune'; $('sessions').replaceChildren();
  $('new-form').hidden = true; $('worktree').replaceChildren(); controls(); schedule(0);
}
function schedule(delay = 2500) { clearTimeout(timer); timer = setTimeout(refresh, delay); }
async function refresh() {
  if (!token || document.hidden) return;
  if (polling) { schedule(); return; }
  polling = true;
  const version = generation, project = $('project').value;
  try {
    const data = await api('sessions', null, {project});
    if (version !== generation) return;
    $('sessions').replaceChildren();
    for (const row of data.sessions) {
      const button = document.createElement('button');
      button.className = 'session' + (selected?.id === row.id ? ' active' : '');
      const title = document.createElement('strong'); title.textContent = row.name;
      const detail = document.createElement('small'); detail.textContent = `${row.kind} · ${row.status} · ${row.worktree}`;
      button.append(title, detail); button.onclick = () => selectSession(row); $('sessions').append(button);
    }
    if (!data.sessions.length) $('sessions').textContent = 'Nicio sesiune activă. Pornește un agent.';
    if (selected && !data.sessions.some(r => r.id === selected.id)) {
      saveDraft(); selected = null; $('output').textContent = 'Sesiunea nu mai este disponibilă.'; controls();
    }
    if (selected) {
      const row = data.sessions.find(r => r.id === selected.id);
      $('session-title').textContent = row.name;
      const outputData = await api('output', null, {project, id: selected.id});
      if (version !== generation) return;
      const output = $('output'), bottom = output.scrollHeight - output.scrollTop - output.clientHeight < 60;
      if (output.textContent !== outputData.output) output.textContent = outputData.output;
      if (bottom) output.scrollTop = output.scrollHeight;
    }
    notice('Conectat · ' + new Date().toLocaleTimeString('ro-RO'));
  } catch (error) { if (version === generation) notice('Reconectare automată: ' + error.message); }
  finally { polling = false; if (token) schedule(); }
}
$('login').onsubmit = async event => {
  event.preventDefault(); token = $('token').value.trim();
  try { await connect(); } catch (error) { notice(error.message); }
};
$('logout').onclick = logout;
$('project').onchange = changeProject;
$('message').oninput = saveDraft;
$('new-toggle').onclick = async () => {
  $('new-form').hidden = !$('new-form').hidden;
  if ($('new-form').hidden) return;
  const version = generation;
  try {
    const data = await api('worktrees', null, {project: $('project').value});
    if (version === generation) $('worktree').replaceChildren(...data.worktrees.map(w => option(w.path, `${w.branch} · ${w.sessions.length} sesiuni`)));
  } catch (error) { notice(error.message); }
};
async function action(path, body, done) {
  if (busy) return;
  busy = true; controls(); const version = generation;
  $('action-status').textContent = 'Se transmite…';
  try {
    await api(path, body);
    if (version === generation) { done(); $('action-status').textContent = 'Instrucțiune transmisă.'; }
  } catch (error) {
    if (version === generation) $('action-status').textContent = error.message + ' Mesajul a fost păstrat. Verifică sesiunile și outputul înainte de a reîncerca.';
  } finally { busy = false; controls(); schedule(1000); }
}
$('send-form').onsubmit = event => {
  event.preventDefault(); if (!selected || !$('message').value.trim()) return;
  action('send', {project: $('project').value, id: selected.id, text: $('message').value}, () => {
    $('message').value = ''; saveDraft();
  });
};
$('new-form').onsubmit = event => {
  event.preventDefault();
  action('new', {project: $('project').value, worktree: $('worktree').value, kind: $('kind').value, text: $('prompt').value}, () => {
    $('prompt').value = ''; $('new-form').hidden = true;
  });
};
document.addEventListener('visibilitychange', () => { if (!document.hidden) schedule(0); else clearTimeout(timer); });
window.addEventListener('online', () => schedule(0));
if ('serviceWorker' in navigator) navigator.serviceWorker.register('/sw.js').catch(() => {});
if (token) connect().catch(error => notice(error.message));
