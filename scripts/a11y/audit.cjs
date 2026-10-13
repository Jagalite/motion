// Accessibility audit (completion 6D): axe-core over every presentation page
// served by `crates/motion-ui/tests/a11y_serve.rs`, in Electron's Chromium,
// with each page's first Tab stop. Run with the Electron runtime:
//
//   MOTION_A11Y_BASE=http://127.0.0.1:PORT AXE_CORE_JS=/path/to/axe.min.js \
//   MOTION_A11Y_REPORT=report.json electron scripts/a11y/audit.cjs
const {app, BrowserWindow} = require('electron');
const {readFileSync, writeFileSync} = require('node:fs');
const PAGES = ['/', '/library/lib1', '/item/item1', '/item/item1/manage', '/search?q=midnight', '/libraries', '/manage',
  '/play/tl1', '/profiles', '/sources', '/matches', '/processing', '/downloads', '/diagnostics', '/organize'];
const base = process.env.MOTION_A11Y_BASE ?? '';
// Or explicit pages (e.g. the desktop chrome as a file URL), comma-separated.
const pages = process.env.MOTION_A11Y_URLS ? process.env.MOTION_A11Y_URLS.split(',') : PAGES;
const axe = readFileSync(process.env.AXE_CORE_JS, 'utf8');
app.whenReady().then(async () => {
  const window = new BrowserWindow({show: false, width: 1280, height: 900, webPreferences: {sandbox: true}});
  const report = {axe_version: null, pages: {}};
  for (const page of pages) {
    try {
      await window.loadURL(base + page);
      // Optionally reveal panels a script shows later (choosers, prompts).
      if (process.env.MOTION_A11Y_REVEAL === '1') {
        await window.webContents.executeJavaScript(`for (const e of document.querySelectorAll('[hidden]')) e.hidden = false`);
      }
      await window.webContents.executeJavaScript(axe);
      const result = await window.webContents.executeJavaScript(`axe.run(document, {runOnly: {type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'best-practice']}})
        .then(r => ({version: axe.version, violations: r.violations.map(v => ({id: v.id, impact: v.impact, help: v.help,
          nodes: v.nodes.slice(0, 5).map(n => ({target: n.target.join(' '), summary: n.failureSummary}))}))}))`);
      report.axe_version = result.version;
      // The first Tab stop, as a keyboard user meets the page.
      window.webContents.focus();
      window.webContents.sendInputEvent({type: 'keyDown', keyCode: 'Tab'});
      window.webContents.sendInputEvent({type: 'keyUp', keyCode: 'Tab'});
      await new Promise(r => setTimeout(r, 100));
      const first = await window.webContents.executeJavaScript(`(() => { const e = document.activeElement; return e ? (e.tagName + (e.className ? '.' + e.className : '') + ' ' + (e.textContent || '').trim().slice(0, 40)) : null; })()`);
      report.pages[page] = {violations: result.violations, first_tab_stop: first};
    } catch (error) {
      report.pages[page] = {error: String(error)};
    }
  }
  writeFileSync(process.env.MOTION_A11Y_REPORT, JSON.stringify(report, null, 2) + '\n');
  app.exit(0);
});
