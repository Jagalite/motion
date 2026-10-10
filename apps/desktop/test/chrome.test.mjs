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
