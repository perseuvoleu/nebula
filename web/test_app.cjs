const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');

// Execute the actual browser script with a minimal DOM and deterministic API.
function harness(outputResponse = async () => ({output: 'Agent response\nNext line'})) {
  function element() {
    return {value: '', textContent: '', children: [], scrollHeight: 100, scrollTop: 0, clientHeight: 100,
      append(...items) {this.children.push(...items);},
      replaceChildren(...items) {this.children = items;}, querySelectorAll() {return [];}};
  }
  const nodes = new Map(), calls = [];
  const get = id => {if (!nodes.has(id)) nodes.set(id, element()); return nodes.get(id);};
  const row = {id: 'one', name: 'Mobile test', kind: 'pi', status: 'finished', worktree: 'main'};
  get('project').value = 'nebula';
  get('output').textContent = 'Se încarcă…';
  const sandbox = vm.createContext({
    document: {hidden: false, getElementById: get, createElement: element, addEventListener() {}},
    navigator: {}, window: {addEventListener() {}},
    sessionStorage: {getItem() {return '';}, setItem() {}, removeItem() {}},
    setTimeout() {return 1;}, clearTimeout() {}, URLSearchParams, AbortSignal,
    fetch: async url => {
      calls.push(url);
      return {ok: true, status: 200, json: async () => url.startsWith('/api/output')
        ? outputResponse() : {sessions: [row]}};
    },
  });
  const source = fs.readFileSync(path.join(__dirname, 'static/app.js'), 'utf8');
  vm.runInContext(source + '\nglobalThis.runRefresh = refresh; globalThis.choose = selectSession;', sandbox);
  vm.runInContext('token = "test-token";', sandbox);
  sandbox.choose(row);
  return {sandbox, get, calls};
}

test('selecting an existing session requests and displays its output', async () => {
  const {sandbox, get, calls} = harness();
  await sandbox.runRefresh();
  assert.equal(get('output').textContent, 'Agent response\nNext line');
  assert.equal(calls.length, 2, 'both sessions and output endpoints must be called');
  assert.match(calls[1], /\/api\/output\?.*id=one/);
  assert.match(get('notice').textContent, /^Conectat/);
  assert.equal(get('session-title').textContent, 'Mobile test');
});

test('subsequent polls replace old output with new response', async () => {
  let count = 0;
  const {sandbox, get} = harness(async () => ({output: `Response ${++count}`}));
  await sandbox.runRefresh();
  await sandbox.runRefresh();
  assert.equal(get('output').textContent, 'Response 2');
});

test('a response for a previous selection cannot overwrite the new selection', async () => {
  let resolve, started;
  const ready = new Promise(r => {started = r;});
  const {sandbox, get} = harness(() => {started(); return new Promise(r => {resolve = r;});});
  const polling = sandbox.runRefresh();
  // A bounded race also makes the pre-fix missing output request fail promptly.
  await Promise.race([ready, polling.then(() => {throw new Error('No output request made');})]);
  sandbox.choose({id: 'two', name: 'Other session'});
  resolve({output: 'Stale output'});
  await polling;
  assert.equal(get('output').textContent, 'Se încarcă…');
  assert.equal(get('session-title').textContent, 'Other session');
});
