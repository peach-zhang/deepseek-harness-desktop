window.__ModuleLoader__.load({
  id: "@deepseek-ai/dsh-plugin-deepseek-usage",
  factory: (require) => {
    var module = { exports: {} };
    var exports = module.exports;
    Object.defineProperty(exports, Symbol.toStringTag, { value: "Module" });

    var React = require("react");
    var createElement = React.createElement;

    var css = [
      ".dshu-trigger{box-sizing:border-box;cursor:pointer;width:100%;height:34px;color:var(--dsw-alias-label-primary,#111);background:transparent;border:none;border-radius:12px;flex:none;display:flex;align-items:center;gap:8px;padding:6px 10px;font-family:inherit;font-size:14px}",
      ".dshu-trigger:hover{background:var(--dsw-alias-interactive-bg-hover,#f0f0f0)}",
      ".dshu-trigger.dshu-rail{justify-content:center;gap:0;width:36px;height:36px;padding:0}",
      ".dshu-trigger.dshu-rail .dshu-status-dot{position:absolute;right:5px;bottom:5px}",
      ".dshu-trigger-label{white-space:nowrap;overflow:hidden}",
      ".dshu-amount{margin-left:auto;flex:none;background:rgba(46,160,90,.16);color:#2ea05a;border-radius:999px;padding:1px 8px;font-size:12px;line-height:18px;font-variant-numeric:tabular-nums;white-space:nowrap}",
      ".dshu-amount-bad{background:rgba(211,60,60,.16);color:var(--dsw-alias-state-error-primary,#d33)}",
      ".dshu-status-dot{width:8px;height:8px;border-radius:50%;flex:none;display:inline-block}",
      ".dshu-status-dot.dshu-dot-ok{background:#2ea05a}",
      ".dshu-status-dot.dshu-dot-bad{background:var(--dsw-alias-state-error-primary,#d33)}",
      ".dshu-status-dot.dshu-dot-wait{background:var(--dsw-alias-label-tertiary,#999)}"
    ].join("\n");

    var tagId = "@deepseek-ai/dsh-plugin-deepseek-usage/usage.css";
    if (typeof document !== "undefined" && document.querySelector('style[data-plugin-css="' + tagId + '"]') === null) {
      var tag = document.createElement("style");
      tag.dataset.plugin = "@deepseek-ai/dsh-plugin-deepseek-usage";
      tag.dataset.pluginCss = tagId;
      tag.textContent = css;
      document.head.appendChild(tag);
    }

    function fmt(value) {
      if (value === undefined || value === null || value === "") return "—";
      return String(value);
    }

    /** Sidebar footer button: shows the DeepSeek account balance directly. */
    function BalanceButton(props) {
      var wide = props.wide;
      var stateRef = React.useState({ phase: "loading", total: null, currency: null, available: false, error: null });
      var state = stateRef[0];
      var setState = stateRef[1];

      var load = React.useCallback(function () {
        fetch("/deepseek-usage", { method: "GET", cache: "no-store" })
          .then(function (r) { return r.json(); })
          .then(function (j) {
            if (j && j.ok) {
              var data = j.data || {};
              var infos = Array.isArray(data.balance_infos) ? data.balance_infos : [];
              var first = infos.length > 0 ? infos[0] : null;
              setState({
                phase: "ready",
                total: first && first.total_balance !== undefined ? String(first.total_balance) : null,
                currency: first && first.currency ? String(first.currency) : null,
                available: data.is_available !== false,
                error: null
              });
            } else {
              setState({ phase: "error", total: null, currency: null, available: false, error: (j && (j.message || j.error)) || "未知错误" });
            }
          })
          .catch(function (e) {
            setState({ phase: "error", total: null, currency: null, available: false, error: (e && e.message) || String(e) });
          });
      }, []);

      React.useEffect(function () {
        load();
        var timer = window.setInterval(load, 60000);
        return function () {
          window.clearInterval(timer);
        };
      }, [load]);

      var phase = state.phase;
      var amount = state.total;
      var titleText = "DeepSeek 余额";
      if (phase === "error") titleText = "余额查询失败：" + state.error;
      else if (phase === "ready" && amount !== null) titleText = "DeepSeek 余额：" + amount + (state.currency ? " " + state.currency : "") + (state.available ? "（可用）" : "（不可用）");
      else if (phase === "ready") titleText = "DeepSeek 余额：暂无数据";
      else titleText = "DeepSeek 余额：加载中…";

      var dotClass = phase === "error" || (phase === "ready" && !state.available) ? "dshu-dot-bad"
        : phase === "ready" && amount !== null ? "dshu-dot-ok"
        : "dshu-dot-wait";

      return createElement(
        "button",
        {
          type: "button",
          className: "dshu-trigger" + (wide ? "" : " dshu-rail"),
          "aria-label": titleText,
          title: titleText,
          onClick: load
        },
        createElement(
          "svg",
          { viewBox: "0 0 16 16", width: wide ? 14 : 18, height: wide ? 14 : 18, "aria-hidden": true },
          createElement("path", { d: "M2 13h12M3 9h4l2-4 2 2h4", stroke: "currentColor", strokeWidth: "1.5", fill: "none", strokeLinecap: "round", strokeLinejoin: "round" })
        ),
        wide ? createElement("span", { className: "dshu-trigger-label" }, "用量") : null,
        !wide ? createElement("span", { className: "dshu-status-dot " + dotClass }) : null,
        wide && phase === "ready" && amount !== null
          ? createElement("span", { className: "dshu-amount" + (!state.available ? " dshu-amount-bad" : "") }, amount + (state.currency ? " " + state.currency : ""))
          : null
      );
    }

    /** Services required by this client plugin. */
    var inject = ["slots"];

    function apply(ctx) {
      ctx.slots.inject("sidebar.footer.action", function () {
        return ctx.slots.register({
          name: "sidebar.footer.action",
          id: "deepseek-usage",
          order: 10
        }, BalanceButton);
      });
    }

    exports.apply = apply;
    exports.inject = inject;
    return module.exports;
  }
});
