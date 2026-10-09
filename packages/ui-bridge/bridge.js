// Motion UI bridge: small external module loaded on every server-rendered page.
// It only adds immediate DOM behaviour; it holds no catalog or viewing state
// and never evaluates strings as code (the page CSP forbids it anyway).

// Move focus to an error panel so keyboard and screen-reader users notice it.
const alert = document.querySelector('main [role="alert"]');
if (alert) {
  alert.setAttribute('tabindex', '-1');
  alert.focus({preventScroll: true});
}
