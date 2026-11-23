// Property inspector: the Stream Deck PI socket, settings bound to [data-setting] fields, and the
// scope and member lists the plugin sends.
'use strict';

let socket;
let context;
let action;
let settings = {};
let lists = { scopes: [], members: [] };

const fields = () => [...document.querySelectorAll('[data-setting]')];

function send(event, payload) {
  if (socket && socket.readyState === WebSocket.OPEN) socket.send(JSON.stringify({ event, context, action, payload }));
}

function option(value, text) {
  const o = document.createElement('option');
  o.value = value;
  o.textContent = text;
  return o;
}

function fill() {
  for (const el of document.querySelectorAll('select[data-list]')) {
    const saved = el.multiple ? (settings[el.dataset.setting] || []) : settings[el.dataset.setting];
    el.replaceChildren();
    if (el.dataset.list === 'scopes') {
      if (!el.multiple && !lists.scopes.length) el.append(option('fleet', 'Whole fleet'));
      for (const s of lists.scopes) {
        if (el.multiple && s.id === 'fleet') continue;
        el.append(option(s.id, `${'  '.repeat(s.depth)}${s.depth === 0 ? 'Whole fleet: ' : ''}${s.name}`));
      }
    } else {
      el.append(option('', 'Pick a member'));
      for (const m of lists.members) el.append(option(m.member, m.handle));
    }
    const wanted = el.multiple ? saved : [saved ?? (el.dataset.list === 'scopes' ? 'fleet' : '')];
    for (const v of wanted) {
      if (v && ![...el.options].some((o) => o.value === v)) {
        el.append(option(v, `${el.dataset.list === 'members' ? settings.handle || v : v} (not visible now)`));
      }
    }
    for (const o of el.options) o.selected = wanted.includes(o.value);
  }
}

function show() {
  for (const el of fields()) {
    const v = settings[el.dataset.setting];
    if (el.type === 'checkbox') el.checked = v === undefined ? el.dataset.default === 'true' : v === true;
    else if (el.tagName !== 'SELECT') el.value = v ?? el.dataset.default ?? '';
  }
  fill();
}

function save() {
  for (const el of fields()) {
    const name = el.dataset.setting;
    if (el.type === 'checkbox') settings[name] = el.checked;
    else if (el.type === 'number') settings[name] = Number(el.value);
    else if (el.multiple) settings[name] = [...el.selectedOptions].map((o) => o.value);
    else settings[name] = el.value;
    if (el.dataset.list === 'members') settings.handle = el.selectedOptions[0] && el.value ? el.selectedOptions[0].textContent : '';
  }
  send('setSettings', settings);
}

window.connectElgatoStreamDeckSocket = (port, uuid, registerEvent, _info, actionInfo) => {
  context = uuid;
  const info = JSON.parse(actionInfo);
  action = info.action;
  settings = (info.payload && info.payload.settings) || {};
  socket = new WebSocket(`ws://127.0.0.1:${port}`);
  socket.onopen = () => {
    socket.send(JSON.stringify({ event: registerEvent, uuid }));
    send('sendToPlugin', { want: 'lists' });
  };
  socket.onmessage = (e) => {
    const msg = JSON.parse(e.data);
    if (msg.event === 'sendToPropertyInspector' && msg.payload) {
      lists = { scopes: msg.payload.scopes || [], members: msg.payload.members || [] };
      fill();
    } else if (msg.event === 'didReceiveSettings') {
      settings = (msg.payload && msg.payload.settings) || {};
      show();
    }
  };
  show();
  for (const el of fields()) el.addEventListener('change', save);
};
