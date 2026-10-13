import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {runInNewContext} from 'node:vm';
test('disconnect save failures remain visible and the control can be retried', async () => {
  const nodes = new Map();
  const node = id => {
    if (!nodes.has(id)) nodes.set(id, {listeners: {}, attributes: {}, addEventListener(name, callback) { this.listeners[name] = callback; }, setAttribute(name, value) { this.attributes[name] = value; }});
    return nodes.get(id);
  };
  let reject;
  runInNewContext(readFileSync(new URL('../src/chrome.js', import.meta.url), 'utf8'), {
    document: {getElementById: node}, window: {motionHost: {onStatus() {}, disconnect: () => new Promise((_, no) => { reject = no; })}},
  });
  const button = node('disconnect'), event = {currentTarget: button};
  const pending = button.listeners.click(event);
  event.currentTarget = null;
  assert.equal(button.disabled, true);
  reject(new Error('Cannot save pending progress'));
  await pending;
  assert.equal(button.disabled, false);
  assert.equal(node('status').attributes.role, 'alert');
  assert.equal(node('status').textContent, 'Cannot save pending progress');
});

test('the offline library choice follows roots saved or removed after startup', async () => {
  const nodes = new Map();
  const node = id => {
    if (!nodes.has(id)) nodes.set(id, {listeners: {}, attributes: {}, children: [], hidden: true,
      addEventListener(name, callback) { this.listeners[name] = callback; }, setAttribute(name, value) { this.attributes[name] = value; },
      replaceChildren(...children) {
        this.children = children;
        const chosen = children.find(child => child.selected) ?? children[0];
        this.value = chosen ? chosen.value : '';
      }});
    return nodes.get(id);
  };
  const root = (name, current = false) => ({name, label: name, titles: 1, current});
  let roots = [root('a'), root('b', true)];
  const opened = [];
  runInNewContext(readFileSync(new URL('../src/chrome.js', import.meta.url), 'utf8'), {
    document: {getElementById: node, createElement: () => ({})},
    window: {motionHost: {onStatus() {}, offlineRoots: async () => roots,
      openDownloads: async name => { if (name !== undefined && !roots.some(r => r.name === name)) throw new Error('gone'); opened.push(name); }}},
  });
  const settle = () => new Promise(resolve => setImmediate(resolve));
  await settle();
  const choice = node('offline-root'), label = node('offline-root-label'), button = node('downloads');
  assert.equal(label.hidden, false);
  assert.equal(choice.value, 'b');
  choice.value = 'a';
  // The chosen root is removed and another saved after startup: the open
  // re-reads the roots, so it neither submits the removed root nor hides the new one.
  roots = [root('b', true), root('c')];
  await button.listeners.click({currentTarget: button});
  assert.deepEqual(opened, ['b']);
  assert.deepEqual(choice.children.map(o => o.value), ['b', 'c']);
  // A still-valid selection is kept across refreshes.
  choice.value = 'c';
  await choice.listeners.focus();
  await settle();
  assert.equal(choice.value, 'c');
  await button.listeners.click({currentTarget: button});
  assert.deepEqual(opened, ['b', 'c']);
  // One library left (the current one was emptied): the choice is hidden
  // but the surviving root is still named, never the emptied current one.
  roots = [root('a')];
  await button.listeners.click({currentTarget: button});
  assert.equal(label.hidden, true);
  assert.deepEqual(opened, ['b', 'c', 'a']);
  // No saved library: nothing is named and the host decides.
  roots = [];
  await button.listeners.click({currentTarget: button});
  assert.deepEqual(opened, ['b', 'c', 'a', undefined]);
});

test('progress from before a server restore is offered for an explicit choice', async () => {
  const nodes = new Map();
  const element = (tag = 'div') => ({tag, children: [], listeners: {}, attributes: {}, hidden: true, disabled: false,
    addEventListener(name, callback) { this.listeners[name] = callback; }, setAttribute(name, value) { this.attributes[name] = value; },
    replaceChildren(...children) { this.children = children; }, append(...children) { this.children.push(...children); },
    querySelectorAll(selector) { return selector === 'button' ? this.children.filter(c => c?.tag === 'button') : []; }});
  const node = id => { if (!nodes.has(id)) nodes.set(id, element()); return nodes.get(id); };
  let choices = [{root: 'a'.repeat(32), profile_id: 'default', events: 3, titles: 2}];
  const chosen = [];
  const statusHandlers = [];
  let layouts = 0;
  runInNewContext(readFileSync(new URL('../src/chrome.js', import.meta.url), 'utf8'), {
    document: {getElementById: node, createElement: tag => element(tag)},
    window: {motionHost: {onStatus: callback => statusHandlers.push(callback), offlineRoots: async () => [],
      offlineRestore: async () => choices,
      chooseOfflineRestore: async (root, choice) => { chosen.push([root, choice]); choices = []; return {discarded: 0, reapplied: 3, dropped: 1}; },
      layoutChanged: async () => { layouts++; }}},
  });
  const settle = () => new Promise(resolve => setImmediate(resolve));
  // The agent's status after a refused sync refreshes the choices.
  statusHandlers[0]('The server was restored from a backup.');
  await settle(); await settle();
  assert.equal(node('offline-restore').hidden, false);
  assert.ok(layouts > 0, 'the library view is placed below the revealed panel');
  const item = node('offline-restore-list').children[0];
  assert.match(item.children[0].textContent, /3 saved positions for 2 titles/);
  const [keep, discard] = item.querySelectorAll('button');
  assert.deepEqual([keep.textContent, discard.textContent], ['Keep my offline progress', 'Discard it']);
  await keep.onclick();
  await settle();
  assert.deepEqual(chosen, [['a'.repeat(32), 'reapply']]);
  assert.match(node('status').textContent, /Kept 3 saved positions; 1 belonged to titles no longer available/);
  assert.equal(node('offline-restore').hidden, true, 'nothing left to choose');
});
