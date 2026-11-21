'use strict';
const menu = document.querySelector('.menu-toggle');
const nav = document.getElementById('main-nav');
if (menu && nav) {
  const close = () => { menu.setAttribute('aria-expanded', 'false'); nav.classList.remove('is-open'); };
  menu.addEventListener('click', () => { const open = menu.getAttribute('aria-expanded') !== 'true'; menu.setAttribute('aria-expanded', String(open)); nav.classList.toggle('is-open', open); });
  nav.addEventListener('click', event => { if (event.target.closest('a')) close(); });
  document.addEventListener('keydown', event => { if (event.key === 'Escape' && menu.getAttribute('aria-expanded') === 'true') { close(); menu.focus(); } });
}
const tabs = [...document.querySelectorAll('[data-layer]')];
function selectLayer(tab) {
  tabs.forEach(item => { const active = item === tab; item.setAttribute('aria-selected', String(active)); item.tabIndex = active ? 0 : -1; document.getElementById(item.getAttribute('aria-controls')).hidden = !active; });
}
tabs.forEach((tab, index) => {
  tab.addEventListener('click', () => selectLayer(tab));
  tab.addEventListener('keydown', event => {
    let next;
    if (event.key === 'ArrowRight' || event.key === 'ArrowDown') next = (index + 1) % tabs.length;
    if (event.key === 'ArrowLeft' || event.key === 'ArrowUp') next = (index - 1 + tabs.length) % tabs.length;
    if (event.key === 'Home') next = 0;
    if (event.key === 'End') next = tabs.length - 1;
    if (next !== undefined) { event.preventDefault(); selectLayer(tabs[next]); tabs[next].focus(); }
  });
});
const filters = [...document.querySelectorAll('[data-filter]')];
const rows = [...document.querySelectorAll('[data-status]')];
filters.forEach(button => button.addEventListener('click', () => {
  const filter = button.dataset.filter;
  filters.forEach(item => item.setAttribute('aria-pressed', String(item === button)));
  let count = 0;
  rows.forEach(row => { row.hidden = filter !== 'all' && row.dataset.status !== filter; if (!row.hidden) count++; });
  const isGerman = document.documentElement.lang === 'de';
  document.getElementById('status-count').textContent = `${count} ${isGerman ? (count === 1 ? 'Bereich' : 'Bereiche') : (count === 1 ? 'area' : 'areas')}`;
}));

// Locale is explicit in the URL; no browser detection or stored preference overrides the English root.
for (const link of document.querySelectorAll('.language-switch a')) {
  link.addEventListener('click', () => { link.hash = window.location.hash; });
}
