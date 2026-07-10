// GENERATED FROM odyssey — DO NOT EDIT
/*! odyssey-canary v1.2.0-canary.1 · public shell/profile enhancement */
(function (window, document) {
  'use strict';
  var Odyssey = window.Odyssey;
  if (!Odyssey || Odyssey.canary) return;

  var RELEASE = '1.2.0-canary.1';
  var PROFILES = [
    'ai', 'communication', 'content', 'control', 'data', 'developer', 'identity',
    'knowledge', 'networking', 'observability', 'portal', 'productivity', 'public',
    'security'
  ];
  var STATUSES = {
    ok: 'operational', healthy: 'operational', operational: 'operational',
    warn: 'degraded', degraded: 'degraded',
    down: 'outage', outage: 'outage',
    maintenance: 'maintenance', unknown: 'unknown'
  };

  function includes(list, value) { return list.indexOf(value) !== -1; }
  function select(root, selector) {
    root = root || document;
    var found = Array.prototype.slice.call(root.querySelectorAll(selector));
    if (root.matches && root.matches(selector)) found.unshift(root);
    return found;
  }
  function samePage(link) {
    try {
      var target = new window.URL(link.href, window.location.href);
      return target.origin === window.location.origin &&
        target.pathname.replace(/\/$/, '') === window.location.pathname.replace(/\/$/, '') &&
        (!target.search || target.search === window.location.search);
    } catch (error) { return false; }
  }
  function markCurrent(nav) {
    if (nav.querySelector('[aria-current="page"]')) return;
    var links = Array.prototype.slice.call(nav.querySelectorAll('a[href]'));
    var current = links.filter(samePage)[0];
    if (!current) return;
    current.classList.add('is-active');
    current.setAttribute('aria-current', 'page');
  }
  function initShell(root) {
    select(root, '[data-ody-profile]').forEach(function (profileRoot) {
      var profile = profileRoot.getAttribute('data-ody-profile');
      if (!includes(PROFILES, profile)) return;
      profileRoot.setAttribute('data-ody-profile-ready', '');
      profileRoot.setAttribute('data-ody-release', RELEASE);
    });
    select(root, '[data-ody-shell-nav]').forEach(markCurrent);
    select(root, '[data-ody-status]').forEach(function (signal) {
      var raw = signal.getAttribute('data-ody-status');
      var status = STATUSES[raw] || 'unknown';
      signal.setAttribute('data-ody-status', status);
      signal.setAttribute('data-ody-status-ready', '');
    });
  }

  var previousInit = Odyssey.init;
  Odyssey.init = function (root) {
    if (previousInit) previousInit.call(Odyssey, root);
    initShell(root);
    return Odyssey;
  };
  Odyssey.version = RELEASE;
  Odyssey.canary = Object.freeze({
    release: RELEASE,
    profiles: Object.freeze(PROFILES.slice()),
    initShell: initShell
  });

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', function () { initShell(document); });
  } else {
    initShell(document);
  }
})(window, document);
