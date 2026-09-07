/*! beacon status-page v1 — UTC clock tick + tile detail placement (progressive only) */
(function (d, w) {
  "use strict";

  var clock = d.querySelector("[data-clock]");
  if (clock) {
    var pad = function (n) { return (n < 10 ? "0" : "") + n; };
    var tick = function () {
      var now = new Date();
      clock.textContent = pad(now.getUTCHours()) + ":" + pad(now.getUTCMinutes()) + " UTC";
      clock.setAttribute("datetime", now.toISOString());
    };
    tick();
    w.setInterval(tick, 15000);
  }

  // A tile detail is an absolutely positioned popover under its tile. When it would leave the
  // viewport on the right, flip it to the tile's right edge. Without JavaScript the CSS
  // column heuristics and the narrow-viewport sheet keep it readable.
  d.addEventListener(
    "toggle",
    function (event) {
      var tile = event.target;
      if (!tile || !tile.classList || !tile.classList.contains("tile")) return;
      var detail = tile.querySelector(".tile__detail");
      if (!detail) return;
      detail.classList.remove("is-flip");
      if (!tile.open) return;
      var rect = detail.getBoundingClientRect();
      if (rect.right > w.innerWidth - 16) detail.classList.add("is-flip");
    },
    true
  );
})(document, window);
