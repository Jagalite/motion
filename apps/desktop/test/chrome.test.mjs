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
  // One library left: the choice is hidden and no root is named.
  roots = [root('c', true)];
  await button.listeners.click({currentTarget: button});
  assert.equal(label.hidden, true);
  assert.deepEqual(opened, ['b', 'c', undefined]);
});
