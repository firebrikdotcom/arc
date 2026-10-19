// Workbench behaviour: collapsible navigation rail and current-page marking.
// Loaded in <head> without `defer` so a collapsed rail never flashes open.
(function () {
    var root = document.documentElement;
    var KEY = "arc:rail";

    try {
        if (localStorage.getItem(KEY) === "collapsed") root.classList.add("rail-collapsed");
    } catch (_) {
        // Storage can be unavailable (private mode); the rail starts expanded.
    }

    function sync() {
        var collapsed = root.classList.contains("rail-collapsed");
        var label = collapsed ? "Expand navigation" : "Collapse navigation";
        document.querySelectorAll("[data-rail-toggle]").forEach(function (button) {
            button.setAttribute("aria-expanded", String(!collapsed));
            button.setAttribute("aria-label", label);
            button.setAttribute("title", label);
        });
    }

    // Mark the rail link whose href is the longest match for the current path.
    function markCurrent() {
        var path = location.pathname.replace(/\/+$/, "") || "/";
        var best = null;
        var bestLength = -1;
        document.querySelectorAll(".rail nav a[href]").forEach(function (link) {
            link.removeAttribute("aria-current");
            var href = link.getAttribute("href").replace(/\/+$/, "") || "/";
            var matches = path === href || path.indexOf(href + "/") === 0;
            if (matches && href.length > bestLength) {
                best = link;
                bestLength = href.length;
            }
        });
        if (best) best.setAttribute("aria-current", "page");
    }

    document.addEventListener("click", function (event) {
        if (!event.target.closest("[data-rail-toggle]")) return;
        var collapsed = root.classList.toggle("rail-collapsed");
        try {
            localStorage.setItem(KEY, collapsed ? "collapsed" : "expanded");
        } catch (_) {
            // The toggle still works for this page.
        }
        sync();
    });

    function ready() {
        sync();
        markCurrent();
    }

    document.addEventListener("DOMContentLoaded", ready);
    // Turbo Drive swaps <body> without reloading this script.
    document.addEventListener("turbo:load", ready);
})();
