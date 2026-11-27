/**
 * QWebChannel 兼容层 (Rust/Tauri 版)
 * ============================================
 *
 * 目的: 让现有的 `web/index.html` (2212 行, 零改动) 里的
 * `backend.xxx(args..., callback)` 调用在 Tauri 下原样工作。
 *
 * 原理:
 *   - QWebChannel 里 `new QWebChannel(qt.webChannelTransport, function(channel) { ... })`
 *     会在回调里给出 `channel.objects.backend`, 上面有各个 Slot 方法。
 *   - 本 shim 提供一个假的 `qt.webChannelTransport` 与 `QWebChannel` 构造器,
 *     回调时立刻 (异步) 把 `channel.objects.backend` 填好。
 *   - `backend.xxx` 的实现是把最后一个参数当回调, 调用 invoke 后把结果回填。
 *   - 信号 (`iconScanned` / `iconThumbnailsLoaded` / `iconsLoadFinished` /
 *     `errorOccurred`) 用 `connect(fn)` 订阅, 底层是 Tauri 的事件监听。
 *
 * 【关键实现选择】只依赖 `window.__TAURI_INTERNALS__`, 不依赖 `window.__TAURI__`:
 *
 *   `window.__TAURI__` (全局 API) 由 `app.withGlobalTauri = true` 控制,
 *   但在 Tauri 2.12.x 上该开关存在失效情况 —— 全局 API 脚本清单由 tauri-build
 *   写进 app 的 OUT_DIR, 而 codegen (proc-macro) 从自己的 OUT_DIR 读, 两者不一致,
 *   结果 `plugin_global_api_scripts` 为 None, `window.__TAURI__` 不会被注入。
 *
 *   而 `window.__TAURI_INTERNALS__` 由 core.js (IPC 初始化脚本) **总是**注入,
 *   它提供了底层原语: `invoke` / `transformCallback` / `registerCallback` /
 *   `runCallback` / `convertFileSrc`。所以这里直接基于它实现, 更稳。
 */
(function () {
  'use strict';

  if (window.__QWEBCHANNEL_SHIM_INSTALLED__) return;
  window.__QWEBCHANNEL_SHIM_INSTALLED__ = true;

  // ---- 事件桥接: Tauri listen <-> QWebChannel signal.connect ----------------
  var signalHandlers = {
    iconScanned: [],
    iconThumbnailsLoaded: [],
    iconsLoadFinished: [],
    errorOccurred: [],
    sortApplied: [],
    snapshotRestored: []
  };

  // Tauri 事件名 -> signal 名
  var EVENT_MAP = {
    'icon-scanned': 'iconScanned',
    'icon-thumbnails-loaded': 'iconThumbnailsLoaded',
    'icons-load-finished': 'iconsLoadFinished',
    'error-occurred': 'errorOccurred',
    'sort-applied': 'sortApplied',
    'snapshot-restored': 'snapshotRestored'
  };

  function internals() {
    return window.__TAURI_INTERNALS__ || null;
  }

  /** 调用 Rust 侧的命令。 */
  function invokeTauri(cmd, args) {
    var i = internals();
    if (!i || typeof i.invoke !== 'function') {
      return Promise.reject(new Error('Tauri API 不可用'));
    }
    return i.invoke(cmd, args || {});
  }

  /**
   * 订阅一个 Tauri 事件。
   *
   * 自己实现 (而不是用 window.__TAURI__.event.listen), 因为后者依赖全局 API。
   *
   * 流程与 Tauri 官方的 event.listen 一致:
   *   1. 用 `transformCallback` 注册一个回调, 拿到 callback id
   *   2. 调用 `plugin:event|listen` 命令, 把 { event, target, handler } 传给 Rust
   *   3. Rust 侧 `app.emit(event, payload)` 时, handler 会被调用
   */
  function listenTauri(eventName, handler) {
    var i = internals();
    if (!i || typeof i.transformCallback !== 'function') {
      return Promise.reject(new Error('Tauri 事件 API 不可用'));
    }
    // transformCallback 返回一个 id, Rust 侧通过它把事件回调回来
    var callbackId = i.transformCallback(function (evt) {
      // evt 形如 { event, id, payload }
      var payload = evt && evt.payload !== undefined ? evt.payload : evt;
      handler(payload);
    }, true);

    return i.invoke('plugin:event|listen', {
      event: eventName,
      target: { kind: 'Any' },
      handler: callbackId
    }).then(function () {
      return {
        unregister: function () {
          return i.invoke('plugin:event|unlisten', {
            event: eventName,
            eventId: callbackId
          });
        }
      };
    });
  }

  function setupEventBridge() {
    Object.keys(EVENT_MAP).forEach(function (eventName) {
      listenTauri(eventName, function (payload) {
        var signal = EVENT_MAP[eventName];
        var handlers = signalHandlers[signal] || [];
        handlers.slice().forEach(function (h) {
          try {
            h(payload);
          } catch (e) {
            console.error('[shim] signal handler error (' + signal + '):', e);
          }
        });
      }).catch(function (e) {
        console.error('[shim] 订阅事件失败 (' + eventName + '):', e);
      });
    });
  }

  function makeSignal(name) {
    return {
      connect: function (fn) {
        if (!signalHandlers[name]) signalHandlers[name] = [];
        signalHandlers[name].push(fn);
      },
      disconnect: function (fn) {
        var list = signalHandlers[name] || [];
        var i = list.indexOf(fn);
        if (i >= 0) list.splice(i, 1);
      }
    };
  }

  // ---- backend 对象: 每个 Slot 方法一个 --------------------------------------
  //
  // 约定: 最后一个参数是回调 (QWebChannel 风格), 其余是命令参数。
  // 有些方法没有参数 (如 scan_icons), 此时第一个参数就是回调。
  var COMMAND_METHODS = [
    'scan_icons',
    'get_desktop_info',
    'preview_sort',
    'apply_sort',
    'undo_snapshot',
    'restore_snapshot',
    'apply_custom_order',
    'preview_snapshot',
    'clear_history',
    'get_snapshot_list',
    'get_scan_sources',
    'save_setting',
    'get_setting',
    'get_all_settings',
    'get_app_version',
    'ping'
  ];

  function makeBackend() {
    var backend = {};

    COMMAND_METHODS.forEach(function (name) {
      backend[name] = function () {
        var args = Array.prototype.slice.call(arguments);
        var cb = null;
        if (args.length > 0 && typeof args[args.length - 1] === 'function') {
          cb = args.pop();
        }

        invokeTauri(name, argsToObject(name, args))
          .then(function (result) {
            if (cb) cb(result);
          })
          .catch(function (err) {
            console.error('[shim] ' + name + ' 失败:', err);
            if (cb) cb(defaultFor(name));
            var errorHandlers = signalHandlers['errorOccurred'] || [];
            errorHandlers.slice().forEach(function (h) {
              try { h(String(err)); } catch (_) {}
            });
          });
      };
    });

    // 信号
    backend.iconScanned = makeSignal('iconScanned');
    backend.iconThumbnailsLoaded = makeSignal('iconThumbnailsLoaded');
    backend.iconsLoadFinished = makeSignal('iconsLoadFinished');
    backend.errorOccurred = makeSignal('errorOccurred');
    backend.sortApplied = makeSignal('sortApplied');
    backend.snapshotRestored = makeSignal('snapshotRestored');

    return backend;
  }

  /** 把位置参数转成 Tauri invoke 需要的对象 (参数名与 Rust 命令签名一致)。 */
  function argsToObject(name, args) {
    var map = {
      preview_sort: ['mode', 'direction', 'reverse'],
      apply_sort: ['mode', 'direction', 'reverse'],
      restore_snapshot: ['depth'],
      apply_custom_order: ['order_json', 'direction'],
      preview_snapshot: ['depth'],
      save_setting: ['key', 'value'],
      get_setting: ['key', 'default']
    };
    var names = map[name];
    if (!names) return {};
    var obj = {};
    for (var i = 0; i < names.length; i++) {
      obj[names[i]] = args[i];
    }
    return obj;
  }

  /** 失败时的默认返回值 (与 Python 版的容错行为一致)。 */
  function defaultFor(name) {
    if (name === 'scan_icons' || name === 'get_desktop_info' ||
        name === 'get_all_settings' || name === 'get_snapshot_list' ||
        name === 'get_scan_sources' || name === 'preview_sort' ||
        name === 'preview_snapshot' || name === 'get_app_version') {
      return '';
    }
    return false;
  }

  // ---- 伪造 QWebChannel ------------------------------------------------------
  window.QWebChannel = function (transport, callback) {
    setupEventBridge();
    // 异步回调, 与 QWebChannel 的行为一致 (不会阻塞页面加载)
    setTimeout(function () {
      try {
        callback({ objects: { backend: makeBackend() } });
      } catch (e) {
        console.error('[shim] QWebChannel 回调失败:', e);
      }
    }, 0);
  };

  // 某些代码会检查 qt.webChannelTransport 是否存在
  window.qt = window.qt || {};
  window.qt.webChannelTransport = window.qt.webChannelTransport || {};

  // QWebChannel 的 onTransported 回调 (部分代码路径会用到)
  window.qt.webChannelTransport.ontransported = function () {};

  console.log('[QWebChannel shim] 已安装 (Rust/Tauri 后端)');
})();
