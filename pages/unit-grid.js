// Copied from cic-panel crates/cic-relay/static/unit-grid.js at 40f58f7; change it there first.
// tools/check-unit-grid.sh checks this copy against a cic-panel checkout.

// unit-grid.js: the nested unit grid layout logic, with no DOM access so node --test can load it.
// This file is the source: Hardpoint copies it to pages/unit-grid.js with a header naming the
// cic-panel commit it came from, so change it here first. Layouts hold ids, kinds, positions,
// binds, looks and labels; a unit object holds `children` on its own grid, as many columns as
// the unit is wide. Cells are square and `cell` px on every grid, whatever the window's size.
// The unit tree comes in through an adapter: { byId: Map, kids(id), unitOf(member)? }.
//
// Every unit object is a container: its own people on top, its own grid below. A unit object
// saved without `children` gets its default grid when the layout is used: its child units as
// unit objects, side by side, each with its own default grid in turn. Those defaults are marked
// `auto` and are not saved until something inside them is edited (`touched`); `children: []`
// is a grid emptied on purpose. A default grid, a starter's unit and a unit just added are as tall
// as they naturally are (naturalRows), so nothing in them is hidden; a size saved stays as saved.
(function (root) {
  "use strict";

  const SCHEMA_VERSION = 3;
  const COLUMNS = 24;
  const CELLS = [48, 64, 72, 80, 96];
  const DEFAULT_CELL = 72;
  // Version 2 rows were 54 px.
  const V2_ROW_PX = 54;
  // The narrowest a child unit is made side by side: under 6 cells a member's row cuts its name
  // and seat short.
  const MIN_SIDE = 6;
  // How an object's own title sits; unsaid, a unit on the board is centered and the rest left.
  // Older layouts said it once for every unit (`titles`): migrateLayout moves that onto each unit.
  const TITLES = ["center", "left"];
  const MAX_OBJECTS = 100;
  const LABEL_MAX = 40;
  const MAX_Y = 999;
  const MAX_H = 99;
  const MAX_BYTES = 64 * 1024;
  // How far each object sits in from its cells on every side, so neighbours are twice this apart;
  // both front ends give it to gridstack as its margin. Then the least of a unit's grid kept in
  // view, and what an item's inset and 1 px border take of its height.
  const OBJECT_INSET_PX = 1;
  const GRID_STRIP_PX = 24;
  const ITEM_CHROME_PX = 2 * (OBJECT_INSET_PX + 1);
  // Board objects are at depth 0. A unit holds a grid at depth 2 at most, so wing > squadron >
  // ship, and the ship's grid holds cards at depth 3.
  const MAX_CONTAINER_DEPTH = 2;
  const MAX_DEPTH = 3;

  // binds: what an object may be bound to ("" means event-wide is allowed). inside: may sit in a
  // unit's grid. Sizes are in cells.
  const KINDS = {
    header: { label: "Event header", about: "The event's name, time left and who is connected.", w: 24, h: 1, minW: 8, minH: 1, binds: [""] },
    unit: { label: "Unit", about: "A unit's own people on top and a grid of its child units below.", w: 12, h: 4, minW: 3, minH: 1, binds: ["unit"], looks: ["cards", "condensed"], inside: true },
    member: { label: "Member card", about: "One person: status, ship, health and oxygen.", w: 4, h: 3, minW: 2, minH: 1, binds: ["member"], looks: ["full", "compact"], inside: true },
    ship: { label: "Ship card", about: "A ship's shields, hull and fuel.", w: 4, h: 3, minW: 2, minH: 1, binds: ["ship"], looks: ["full", "compact"], inside: true },
    rollup: { label: "Roll-up", about: "How many are up, downed or dead.", w: 3, h: 2, minW: 2, minH: 1, binds: ["", "unit"], inside: true },
    alerts: { label: "Alerts", about: "Who is downed or dead, newest first.", w: 6, h: 3, minW: 3, minH: 1, binds: ["", "unit"], inside: true },
    unassigned: { label: "Unassigned", about: "Members not placed in a unit yet.", w: 4, h: 3, minW: 3, minH: 1, binds: [""] },
    timeline: { label: "Timeline", about: "Status changes and moves since this page opened.", w: 8, h: 3, minW: 4, minH: 1, binds: [""] },
  };
  const FIELDS = ["id", "kind", "x", "y", "w", "h", "bind", "look", "label", "align", "children"];
  const ID_RE = /^[A-Za-z0-9_-]{1,64}$/;
  const BIND_RE = /^(ship:)?[A-Za-z0-9_-]{1,64}$/;

  /** A unit object with something in its grid. */
  const isContainer = (o) => Boolean(o) && o.kind === "unit" && Array.isArray(o.children) && o.children.length > 0;
  /** Whether an object at `depth` may hold a grid: a unit, not too deep. */
  const mayHold = (o, depth) => Boolean(o) && o.kind === "unit" && depth <= MAX_CONTAINER_DEPTH;
  const hasGrid = (o) => Boolean(o) && o.kind === "unit" && Array.isArray(o.children);
  const cleanLabel = (s) => String(s || "").replace(/[\u0000-\u001f<>]/g, "").trim().slice(0, LABEL_MAX);
  const int = (n, lo, hi) => Math.max(lo, Math.min(hi, Math.round(Number(n) || 0)));
  const clone = (x) => JSON.parse(JSON.stringify(x));
  const bottomOf = (list) => list.reduce((m, o) => Math.max(m, o.y + o.h), 0);

  /** Calls fn(object, depth, parent) for every object, parents before children. */
  function walk(objects, fn, depth, parent) {
    for (const o of objects || []) {
      fn(o, depth || 0, parent || null);
      if (Array.isArray(o.children)) walk(o.children, fn, (depth || 0) + 1, o);
    }
  }

  function count(objects) {
    let n = 0;
    walk(objects, () => { n += 1; });
    return n;
  }

  /** Where an object sits: { object, parent, list, depth }, or null. */
  function find(objects, id) {
    let at = null;
    walk(objects, (o, depth, parent) => {
      if (!at && o.id === id) at = { object: o, parent, list: parent ? parent.children : objects, depth };
    });
    return at;
  }

  /** A saved layout as this version shows it: versions 1 and 2 as version 3, then a layout-wide
   * `titles` setting on each unit (migrateTitles). */
  function migrateLayout(layout) {
    if (!layout || typeof layout !== "object") return layout;
    return migrateTitles(layout.schemaVersion === 1 || layout.schemaVersion === 2 ? migrateV2(layout) : layout);
  }

  /** Versions 1 and 2 as version 3 (layouts.rs upgrades the same way): 72 px cells, rows of 54
   * px scaled to them, and children from their unit's 24 columns to as many as it is wide. A
   * unit without children gets its default grid, as an auto list showed its child units. */
  function migrateV2(layout) {
    const rows = (n) => Math.round((Number(n) || 0) * V2_ROW_PX / DEFAULT_CELL);
    const scale = (list, cols) => (Array.isArray(list) ? list.map((o) => {
      const out = Object.assign({}, o, { y: rows(o.y), h: Math.max(1, rows(o.h)) });
      if (cols !== COLUMNS) {
        out.w = Math.max(1, Math.min(cols, Math.round((Number(o.w) || 0) * cols / COLUMNS)));
        out.x = Math.min(Math.round((Number(o.x) || 0) * cols / COLUMNS), cols - out.w);
      }
      if (Array.isArray(o.children)) out.children = scale(o.children, Math.max(1, Math.min(COLUMNS, Number(out.w) || 1)));
      return out;
    }) : list);
    return Object.assign({}, layout, { schemaVersion: SCHEMA_VERSION, cell: DEFAULT_CELL, objects: scale(layout.objects, COLUMNS) });
  }

  /** A layout's one `titles` setting as each unit's own `align`, where the unit has none (as
   * layouts.rs upgrade): it looks as it did. Other objects kept their titles left, so stay so. */
  function migrateTitles(layout) {
    if (!("titles" in layout)) return layout;
    const all = TITLES.includes(layout.titles) ? layout.titles : null;
    const set = (list) => (Array.isArray(list) ? list.map((o) => {
      const out = Object.assign({}, o);
      if (all && o.kind === "unit" && !TITLES.includes(o.align)) out.align = all;
      if (Array.isArray(o.children)) out.children = set(o.children);
      return out;
    }) : list);
    const out = Object.assign({}, layout, { objects: set(layout.objects) });
    delete out.titles;
    return out;
  }

  /** How an object's title sits: its own setting, else centered for a unit on the board (`depth`
   * 0) and left for the rest. */
  const titleAlign = (o, depth) => (TITLES.includes(o && o.align) ? o.align : o && o.kind === "unit" && !depth ? "center" : "left");

  /** A default grid nobody has edited, all the way down: it is not saved. */
  function untouched(o) {
    if (!hasGrid(o)) return true;
    return Boolean(o.auto) && !o.touched && o.children.every(untouched);
  }

  /** The layout as it is saved: ids, kinds, positions, binds, looks, labels and children only.
   * Kinds a unit may not hold, nesting past the depth rules and objects past the cap go, and
   * default grids nobody edited are left to be made again. */
  function serializeLayout(layout) {
    let left = MAX_OBJECTS;
    const clean = (list, depth, cols) => {
      const out = [];
      for (const o of list || []) {
        const k = KINDS[o.kind];
        if (!k || left <= 0 || (depth > 0 && !k.inside)) continue;
        left -= 1;
        const w = int(o.w, 1, cols);
        const s = { id: String(o.id), kind: o.kind, x: int(o.x, 0, cols - w), y: int(o.y, 0, MAX_Y), w, h: int(o.h, 1, MAX_H) };
        if (typeof o.bind === "string" && o.bind) s.bind = o.bind;
        if (k.looks && k.looks.includes(o.look)) s.look = o.look;
        const label = cleanLabel(o.label);
        if (label) s.label = label;
        if (TITLES.includes(o.align)) s.align = o.align;
        if (mayHold(o, depth) && Array.isArray(o.children) && !untouched(o)) s.children = clean(o.children, depth + 1, w);
        out.push(s);
      }
      return out;
    };
    const objects = clean(layout && layout.objects, 0, COLUMNS);
    const cell = CELLS.includes(layout && layout.cell) ? layout.cell : DEFAULT_CELL;
    return { schemaVersion: SCHEMA_VERSION, columns: COLUMNS, cell, rows: bottomOf(objects), objects };
  }

  /** What the relay (layouts.rs) would refuse, in plain words. */
  function layoutProblems(layout) {
    const p = [];
    if (!layout || layout.schemaVersion !== SCHEMA_VERSION) p.push("unknown schema version");
    if (!layout || layout.columns !== COLUMNS) p.push(`columns must be ${COLUMNS}`);
    if (layout && layout.cell !== undefined && !CELLS.includes(layout.cell)) p.push(`cells are ${CELLS.join(", ")} px`);
    if (layout && layout.titles !== undefined && !TITLES.includes(layout.titles)) p.push(`titles are ${TITLES.join(" or ")}`);
    const objects = (layout && layout.objects) || [];
    if (count(objects) > MAX_OBJECTS) p.push(`more than ${MAX_OBJECTS} objects`);
    const ids = new Set();
    walk(objects, (o, depth, parent) => {
      const k = KINDS[o.kind];
      const cols = parent ? parent.w : COLUMNS;
      if (!k) p.push(`unknown kind ${String(o.kind).slice(0, 20)}`);
      if (!ID_RE.test(o.id) || ids.has(o.id)) p.push("object ids must be unique and plain");
      ids.add(o.id);
      const fits = Number.isInteger(o.x) && Number.isInteger(o.w) && o.w >= 1 && o.x >= 0 && o.x + o.w <= cols;
      if (!fits || !(o.y >= 0 && o.y <= MAX_Y) || !(o.h >= 1 && o.h <= MAX_H)) p.push("an object is outside the grid");
      if (o.bind !== undefined && !BIND_RE.test(o.bind)) p.push("binds are ids only");
      if (o.align !== undefined && !TITLES.includes(o.align)) p.push(`title alignments are ${TITLES.join(" or ")}`);
      if (o.label !== undefined && (typeof o.label !== "string" || o.label.length > LABEL_MAX || /[<>]/.test(o.label))) p.push(`labels are plain text up to ${LABEL_MAX} characters`);
      const extra = Object.keys(o).filter((f) => !FIELDS.includes(f));
      if (extra.length) p.push(`unexpected ${extra.join(", ")}`);
      if (o.children !== undefined) {
        if (o.kind !== "unit") p.push("only a unit holds other objects");
        else if (!Array.isArray(o.children)) p.push("children are a list");
        else if (o.children.length && depth > MAX_CONTAINER_DEPTH) p.push("units nest at most 3 deep");
      }
      if (depth > 0 && k && !k.inside) p.push(`${k.label} stays on the board`);
      if (depth > MAX_DEPTH) p.push("units nest at most 3 deep");
    });
    if (JSON.stringify(layout || {}).length > MAX_BYTES) p.push("layout over 64 KiB");
    return [...new Set(p)];
  }

  /** How many of `n` sit side by side on a grid `cols` wide, and how wide each is: at most 4
   * across, none under MIN_SIDE columns unless the grid is narrower. */
  function sideBySide(n, cols) {
    const across = Math.max(1, Math.min(n, 4, Math.floor(cols / MIN_SIDE)));
    return { across, w: Math.max(1, Math.floor(cols / across)) };
  }

  /** Places objects (with their h set) side by side in rows on a grid `cols` wide, from row `y`. */
  function placeSideBySide(objects, cols, y) {
    const { across, w } = sideBySide(objects.length, cols);
    let top = y || 0;
    for (let i = 0; i < objects.length; i += across) {
      const row = objects.slice(i, i + across);
      row.forEach((o, j) => Object.assign(o, { x: j * w, y: top, w }));
      top += Math.max(...row.map((o) => o.h));
    }
    return objects;
  }

  const boundUnits = (objects) => {
    const bound = new Set();
    walk(objects, (o) => { if (o.kind === "unit" && o.bind) bound.add(o.bind); });
    return bound;
  };

  /** The rows of `cell` px a unit object for `unitId` takes to show everything in full: its own
   * people (`opts.detailsPx(unit, px, look, holds)` px tall at `px` wide), then its default grid,
   * each child unit at its own natural height and the tallest setting each row of them. `opts` =
   * { cols, cell, detailsPx, look?, depth?, skip? }; `depth` is where the object sits. */
  function naturalRows(unitId, tree, opts) {
    const u = tree.byId.get(unitId);
    const cell = opts.cell || DEFAULT_CELL;
    const depth = opts.depth || 0;
    const holds = depth <= MAX_CONTAINER_DEPTH;
    const skip = opts.skip || new Set();
    const kids = u && holds ? tree.kids(unitId).filter((k) => !skip.has(k.id)) : [];
    let rows = unitMinRows(u ? opts.detailsPx(u, opts.cols * cell, opts.look, holds) : 0, false, cell);
    const { across, w } = sideBySide(kids.length, opts.cols);
    const inner = Object.assign({}, opts, { cols: w, depth: depth + 1 });
    for (let i = 0; i < kids.length; i += across) {
      rows += Math.max(...kids.slice(i, i + across).map((k) => naturalRows(k.id, tree, inner)));
    }
    return int(rows, KINDS.unit.minH, MAX_H);
  }

  /** Makes object `o` in `list` `h` rows tall at least; what sits below it moves down together,
   * only as far as it must to clear it. */
  function growIn(list, o, h) {
    if (h <= o.h) return;
    const below = list.filter((x) => x !== o && x.y >= o.y + o.h);
    const by = o.y + h - Math.min(Infinity, ...below.map((x) => x.y));
    if (by > 0) for (const x of below) x.y += by;
    o.h = h;
  }

  /** A unit's default grid: its child units, less those in `opts.skip`, side by side on `opts.cols`
   * columns, at most `opts.max`. `opts` = { cols, look, nextId(), rows(unit, w)?, skip?, max? }. */
  function defaultChildren(unitId, tree, opts) {
    const skip = opts.skip || new Set();
    const kids = tree.kids(unitId).filter((u) => !skip.has(u.id)).slice(0, opts.max === undefined ? Infinity : Math.max(0, opts.max));
    const { w } = sideBySide(kids.length, opts.cols);
    const objects = kids.map((u) => {
      const o = { id: opts.nextId(), kind: "unit", x: 0, y: 0, w, h: opts.rows ? int(opts.rows(u, w), 1, MAX_H) : KINDS.unit.h, bind: u.id };
      if (opts.look) o.look = opts.look;
      return o;
    });
    return placeSideBySide(objects, opts.cols, 0);
  }

  /** Gives every unit object that may hold a grid and has none its default grid, all the way
   * down, in place; units bound anywhere in the layout are not repeated and the cap holds.
   * Returns how many objects were added. `opts` as for defaultChildren, or with `cell` and
   * `detailsPx` (as for naturalRows) in place of `rows`: then each default child unit is as tall
   * as it naturally is, and an object marked `fit` (its grid reset, or just added) grows to show
   * its new grid in full, and the grids around it grow to hold it. */
  function fillDefaults(objects, tree, opts) {
    const skip = boundUnits(objects);
    let left = MAX_OBJECTS - count(objects);
    let added = 0;
    const fitted = [];
    walk(objects, (o, depth) => {
      if (!mayHold(o, depth) || Array.isArray(o.children)) return;
      const known = o.bind && tree.byId.has(o.bind);
      const rows = opts.detailsPx
        ? (u, w) => naturalRows(u.id, tree, { cols: w, cell: opts.cell, detailsPx: opts.detailsPx, look: o.look, depth: depth + 1, skip })
        : opts.rows;
      o.children = known ? defaultChildren(o.bind, tree, Object.assign({}, opts, { cols: o.w, look: o.look, skip, max: left, rows })) : [];
      o.auto = true;
      if (o.fit) fitted.push(o);
      for (const c of o.children) skip.add(c.bind);
      left -= o.children.length;
      added += o.children.length;
    });
    for (const o of fitted) {
      delete o.fit;
      if (opts.detailsPx) fitAround(objects, o, tree, opts);
    }
    return added;
  }

  /** Makes units sized by default exactly as tall as they are drawn, in place: those in a default
   * grid nobody has edited, and with `all` (a starter on show) every unit. `ownPx(o)` is the
   * drawn height of unit object `o`'s header and own people, or null when it is not drawn. A unit
   * takes whole cells: its own part, then its grid's rows, so less than a cell is left below its
   * children. Innermost first, a default grid's units placed side by side again at their new
   * heights. A unit marked `settle` (grown by fitAround) settles too, and what sits below it
   * moves back up as far as it was pushed. Only heights change, so what is drawn (and measured)
   * stays as wide as it was. Returns whether anything moved. */
  function settleRows(objects, ownPx, cell, all) {
    let changed = false;
    const set = (o, k, v) => { if (o[k] !== v) { o[k] = v; changed = true; } };
    const settle = (list, ours) => {
      for (const o of list) {
        if (o.kind !== "unit") continue;
        const auto = Array.isArray(o.children) && untouched(o);
        if (Array.isArray(o.children) && o.children.length) {
          settle(o.children, auto);
          if (auto) {
            const at = new Map(o.children.map((c) => [c.id, [c.x, c.y, c.w]]));
            placeSideBySide(o.children, o.w, 0);
            for (const c of o.children) if (String(at.get(c.id)) !== String([c.x, c.y, c.w])) changed = true;
          }
        }
        const marked = Boolean(o.settle);
        delete o.settle;
        const px = ours || marked ? ownPx(o) : null;
        if (px === null || px === undefined) continue;
        const was = o.h;
        set(o, "h", int(unitMinRows(px, false, cell) + bottomOf(o.children || []), KINDS.unit.minH, MAX_H));
        if (marked && !ours && o.h < was) liftBelow(list, o, was - o.h);
      }
    };
    settle(objects, Boolean(all));
    if (all) {
      // Units on the board keep their order down it, closing the rows they no longer need.
      const was = new Map(objects.map((o) => [o, o.y]));
      const byTop = objects.slice().sort((a, b) => was.get(a) - was.get(b) || a.x - b.x);
      for (const o of byTop) {
        const above = byTop.filter((x) => x !== o && was.get(x) < was.get(o) && x.x < o.x + o.w && o.x < x.x + x.w);
        set(o, "y", above.length ? Math.max(...above.map((x) => x.y + x.h)) : 0);
      }
    }
    return changed;
  }

  /** After `o` in `list` got `by` rows shorter: what sat below it moves up, at most `by` rows and
   * never onto anything. */
  function liftBelow(list, o, by) {
    const bottom = o.y + o.h + by;
    const below = list.filter((x) => x !== o && x.y >= bottom).sort((a, b) => a.y - b.y);
    for (const x of below) {
      const over = list.filter((v) => v !== x && !below.slice(below.indexOf(x)).includes(v) && v.x < x.x + x.w && x.x < v.x + v.w && v.y < x.y);
      x.y = Math.max(x.y - by, ...over.map((v) => v.y + v.h));
    }
  }

  /** Grows unit object `o` to show its people and its grid in full, then each grid it sits in
   * to hold it. One just added (`place`) has no spot yet: only it grows. */
  function fitAround(objects, o, tree, opts) {
    const cell = opts.cell || DEFAULT_CELL;
    const need = (x, depth) => {
      const u = tree.byId.get(x.bind);
      return unitMinRows(u ? opts.detailsPx(u, x.w * cell, x.look, mayHold(x, depth)) : 0, false, cell) + bottomOf(x.children || []);
    };
    for (let at = find(objects, o.id); at; at = at.parent && find(objects, at.parent.id)) {
      if (at.object.kind !== "unit") break;
      growIn(at.list, at.object, int(need(at.object, at.depth), KINDS.unit.minH, MAX_H));
      // Grown from an estimate that errs tall: settleRows makes it exact once it is drawn.
      at.object.settle = true;
      if (at.object.place) break;
    }
  }

  /** Marks the grid of `id` as edited, so it is saved. */
  function touch(objects, id) {
    const at = id && find(objects, id);
    if (at && hasGrid(at.object)) at.object.touched = true;
  }

  /** Marks the grid `id` sits in as edited. */
  function touchParent(objects, id) {
    const at = find(objects, id);
    if (at && at.parent) at.parent.touched = true;
  }

  /** Child units of a unit's grid with no object anywhere in the layout, added at the bottom of
   * it. In a grid that is still the default they join it; elsewhere they are marked `isNew`
   * (never saved until kept); `rows(unit, w)`, when given, sets how tall each is. Returns the
   * added objects. */
  function autoAppend(container, layout, tree, nextId, rows) {
    if (!hasGrid(container) || !container.bind || !(container.children.length || container.auto)) return [];
    const bound = boundUnits(layout.objects);
    const missing = tree.kids(container.bind).filter((u) => !bound.has(u.id));
    if (!missing.length || count(layout.objects) + missing.length > MAX_OBJECTS) return [];
    const fresh = !untouched(container);
    const bottom = bottomOf(container.children);
    const cols = container.w;
    const { w } = sideBySide(container.children.filter((o) => o.kind === "unit").length + missing.length, cols);
    const added = missing.map((u) => {
      const o = { id: nextId(), kind: "unit", x: 0, y: 0, w, h: rows ? int(rows(u, w), 1, MAX_H) : KINDS.unit.h, bind: u.id };
      if (fresh) o.isNew = true;
      if (container.look) o.look = container.look;
      return o;
    });
    const across = Math.max(1, Math.floor(cols / w));
    let top = bottom;
    for (let i = 0; i < added.length; i += across) {
      const row = added.slice(i, i + across);
      row.forEach((o, j) => Object.assign(o, { x: j * w, y: top }));
      top += Math.max(...row.map((o) => o.h));
    }
    container.children.push(...added);
    return added;
  }

  /** The units a layout leaves out: no unit object at any level binds them, an ancestor of
   * them, or one of their descendants. */
  function unplacedUnits(layout, tree) {
    const covered = new Set();
    const subtree = (id) => { covered.add(id); tree.kids(id).forEach((k) => subtree(k.id)); };
    for (const id of boundUnits(layout.objects)) {
      if (!tree.byId.has(id)) continue;
      subtree(id);
      let u = tree.byId.get(id);
      while (u && u.parent) { covered.add(u.parent); u = tree.byId.get(u.parent); }
    }
    return [...tree.byId.values()].filter((u) => !covered.has(u.id));
  }

  /** Where the palette adds: the selected object when it may hold a grid, else the grid it sits
   * in, else the board. Returns { object, depth } with object null for the board. */
  function targetOf(objects, selectedId) {
    const at = selectedId && find(objects, selectedId);
    if (!at) return { object: null, depth: -1 };
    if (mayHold(at.object, at.depth)) return { object: at.object, depth: at.depth };
    if (at.parent) return { object: at.parent, depth: at.depth - 1 };
    return { object: null, depth: -1 };
  }

  /** The kinds the palette offers for a target from targetOf: everything on the board; in a
   * unit's grid, the kinds that may sit inside, a unit only when the unit has child units and a
   * ship card only when there is a ship in it. */
  function holds(target, tree) {
    const c = target && target.object;
    if (!c) return Object.keys(KINDS);
    if (!mayHold(c, target.depth) || target.depth + 1 > MAX_DEPTH) return [];
    const u = c.bind && tree.byId.get(c.bind);
    const subtree = (id) => [tree.byId.get(id), ...tree.kids(id).flatMap((k) => subtree(k.id))].filter(Boolean);
    return Object.keys(KINDS).filter((k) => {
      if (!KINDS[k].inside) return false;
      if (!u) return true;
      if (k === "unit") return tree.kids(u.id).length > 0;
      if (k === "ship") return subtree(u.id).some((x) => x.kind === "ship");
      return true;
    });
  }

  /** The layout with `o` added to the grid of `containerId` (null: the board), placed at the
   * first free spot; null when the cap, the kind or the depth refuses it. */
  function addTo(layout, containerId, o) {
    if (count(layout.objects) >= MAX_OBJECTS) return null;
    const next = clone(layout);
    const item = Object.assign({}, o, { place: true });
    if (o.kind === "unit" && !Array.isArray(o.children)) item.fit = true;
    if (!containerId) {
      item.w = Math.min(item.w, COLUMNS);
      next.objects.push(item);
      return next;
    }
    const at = find(next.objects, containerId);
    const k = KINDS[o.kind];
    if (!at || !mayHold(at.object, at.depth) || !k || !k.inside || at.depth + 1 + height(o) > MAX_DEPTH) return null;
    if (!Array.isArray(at.object.children)) at.object.children = [];
    item.w = Math.min(item.w, at.object.w);
    at.object.children.push(item);
    at.object.touched = true;
    return next;
  }

  /** The layout without `id`; the grid it was in is saved as edited. */
  function removeFrom(layout, id) {
    const next = clone(layout);
    const at = find(next.objects, id);
    if (!at) return next;
    at.list.splice(at.list.indexOf(at.object), 1);
    if (at.parent) at.parent.touched = true;
    return next;
  }

  /** The layout with the grid of `id` back to its default, made again when the layout is used,
   * and the unit marked to grow to show it. */
  function resetInside(layout, id) {
    const next = clone(layout);
    const at = find(next.objects, id);
    if (!at || at.object.kind !== "unit") return null;
    delete at.object.children;
    delete at.object.auto;
    delete at.object.touched;
    at.object.fit = true;
    if (at.parent) at.parent.touched = true;
    return next;
  }

  /** The unit an object belongs in: a unit's parent, a member's unit, a ship card's ship. */
  function homeUnit(o, tree) {
    if (!o.bind) return null;
    if (o.kind === "unit") { const u = tree.byId.get(o.bind); return (u && u.parent) || null; }
    if (o.kind === "member") return tree.unitOf ? tree.unitOf(o.bind) || null : null;
    if (o.kind === "ship") return o.bind.replace(/^ship:/, "");
    return o.bind;
  }

  /** The nearest unit grid for `o`'s home unit or its ancestors, outside `o` itself. */
  function homeContainer(layout, o, tree) {
    const inside = new Set();
    walk([o], (x) => inside.add(x.id));
    const grids = new Map();
    walk(layout.objects, (x, depth) => {
      if (hasGrid(x) && mayHold(x, depth) && x.bind && !inside.has(x.id) && !grids.has(x.bind)) grids.set(x.bind, { object: x, depth });
    });
    for (let id = homeUnit(o, tree); id; id = (tree.byId.get(id) || {}).parent) {
      if (grids.has(id)) return grids.get(id);
    }
    return null;
  }

  /** How many levels an object holds below itself. */
  const height = (o) => (isContainer(o) ? 1 + Math.max(...o.children.map(height)) : 0);

  /** The layout with `id` moved from its grid to the bottom of the board. */
  function moveOut(layout, id) {
    const next = clone(layout);
    const at = find(next.objects, id);
    if (!at || !at.parent) return null;
    at.list.splice(at.list.indexOf(at.object), 1);
    at.parent.touched = true;
    Object.assign(at.object, { x: 0, y: bottomOf(next.objects) });
    next.objects.push(at.object);
    return next;
  }

  /** The layout with `id` moved into its home unit's grid, at the bottom; null when there is no
   * such grid or the move would nest too deep. */
  function putBack(layout, id, tree) {
    const next = clone(layout);
    const at = find(next.objects, id);
    if (!at) return null;
    const home = homeContainer(next, at.object, tree);
    if (!home || home.object === at.parent) return null;
    const k = KINDS[at.object.kind];
    if (!k || !k.inside || home.depth + 1 + height(at.object) > MAX_DEPTH) return null;
    at.list.splice(at.list.indexOf(at.object), 1);
    if (at.parent) at.parent.touched = true;
    Object.assign(at.object, { x: 0, y: bottomOf(home.object.children), w: Math.min(at.object.w, home.object.w) });
    home.object.children.push(at.object);
    home.object.touched = true;
    return next;
  }

  /** The rows of `cell` px a unit object's header and own people (`detailsPx` tall) take in full,
   * plus a strip of its grid when it holds objects. The front ends add the rows down to its
   * lowest child, so a unit is never shorter than what it holds. */
  function unitMinRows(detailsPx, holdsObjects, cell) {
    const px = (Number(detailsPx) || 0) + (holdsObjects ? GRID_STRIP_PX : 0) + ITEM_CHROME_PX;
    return int(Math.ceil(px / (cell || DEFAULT_CELL)), KINDS.unit.minH, MAX_H);
  }

  // A unit kind's level in a structure: the top (fleet, wing, ground force), a group under it,
  // or a ship. A ship is a group in a fleet and a child in a squadron.
  const KIND_LEVEL = { fleet: 0, wing: 0, ground: 0, squadron: 1, squad: 1, ship: 1 };

  /** A unit object's title tier: 0 the top, 1 a group, 2 a unit inside a group. The deepest of
   * where the object sits (`depth`), where its unit sits in the structure and its kind's level,
   * so a fighter in its squadron is a child however it is placed. */
  function titleTier(depth, unit, tree) {
    let up = 0;
    for (let p = unit && tree.byId.get(unit.parent); p && up < 2; p = tree.byId.get(p.parent)) up += 1;
    const kind = unit ? KIND_LEVEL[unit.kind] || 0 : 0;
    return Math.min(2, Math.max(Number(depth) || 0, up, kind));
  }

  /** Our objects as gridstack widget options, sub-grids as `subGridOpts.children` with as many
   * columns as their unit is wide. An object marked `place` (just added) takes the first free
   * spot. */
  function toGridstack(objects, subGridOpts) {
    return (objects || []).map((o) => {
      const k = KINDS[o.kind] || {};
      const w = { id: o.id, x: o.x, y: o.y, w: o.w, h: o.h, minW: k.minW, minH: k.minH };
      if (o.place) w.autoPosition = true;
      if (isContainer(o)) w.subGridOpts = Object.assign({}, subGridOpts || {}, { column: o.w, children: toGridstack(o.children, subGridOpts) });
      return w;
    });
  }

  /** The objects with positions from gridstack nodes ({ id, x, y, w, h }, every grid's). */
  function withPositions(objects, nodes) {
    const at = nodes instanceof Map ? nodes : new Map((nodes || []).map((n) => [n.id, n]));
    return (objects || []).map((o) => {
      const n = at.get(o.id);
      const out = n ? Object.assign({}, o, { x: n.x, y: n.y, w: n.w, h: n.h }) : Object.assign({}, o);
      if (Array.isArray(o.children)) out.children = withPositions(o.children, at);
      return out;
    });
  }

  /** Fits every object inside its grid, in place: a unit made narrower narrows what it holds. */
  function clampTo(objects, cols) {
    for (const o of objects || []) {
      o.w = int(o.w, 1, cols);
      o.x = int(o.x, 0, cols - o.w);
      if (Array.isArray(o.children)) clampTo(o.children, o.w);
    }
    return objects;
  }

  /** The objects without the ones still marked new, or with the mark dropped when `keep`. */
  function settleNew(objects, keep) {
    return (objects || []).filter((o) => keep || !o.isNew).map((o) => {
      const out = Object.assign({}, o);
      delete out.isNew;
      if (Array.isArray(o.children)) out.children = settleNew(o.children, keep);
      return out;
    });
  }

  const api = {
    SCHEMA_VERSION, COLUMNS, CELLS, DEFAULT_CELL, TITLES, OBJECT_INSET_PX, MAX_OBJECTS, LABEL_MAX, MAX_CONTAINER_DEPTH, MAX_DEPTH, KINDS,
    isContainer, mayHold, hasGrid, walk, count, find, migrateLayout, titleAlign, untouched, serializeLayout, layoutProblems,
    unitMinRows, titleTier, naturalRows, settleRows, sideBySide, placeSideBySide, boundUnits, defaultChildren, fillDefaults, touch, touchParent, autoAppend, unplacedUnits,
    targetOf, holds, addTo, removeFrom, resetInside, homeUnit, homeContainer, moveOut, putBack, toGridstack, withPositions, clampTo, settleNew,
  };
  if (typeof module === "object" && module.exports) module.exports = api;
  else root.UnitGrid = api;
})(this);
