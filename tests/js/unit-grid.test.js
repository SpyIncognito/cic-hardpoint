// unit-grid.js, copied from cic-panel crates/cic-relay/tests/js/unit-grid.test.js at 40f58f7 with its require path changed.
const test = require("node:test");
const assert = require("node:assert/strict");
const UG = require("../../pages/unit-grid.js");

const obj = (id, kind, over) => Object.assign({ id, kind, x: 0, y: 0, w: 6, h: 3 }, over || {});
const v3 = (objects) => ({ schemaVersion: 3, columns: 24, cell: 72, rows: 9, objects });

// Fleet f > ships i1, i2; wing w > squadrons s1, s2 > ships f1, f2 under s1; ground g > squad q1;
// a second wing w2 with no squadrons.
const units = [
  { id: "f", kind: "fleet", name: "Capital 1" }, { id: "i1", kind: "ship", name: "Idris 1", parent: "f" }, { id: "i2", kind: "ship", name: "Idris 2", parent: "f" },
  { id: "w", kind: "wing", name: "Wing" }, { id: "s1", kind: "squadron", name: "S1", parent: "w" },
  { id: "s2", kind: "squadron", name: "S2", parent: "w" }, { id: "f1", kind: "ship", name: "F1", parent: "s1" },
  { id: "f2", kind: "ship", name: "F2", parent: "s1" }, { id: "g", kind: "ground", name: "Ground" },
  { id: "q1", kind: "squad", name: "Alpha", parent: "g" }, { id: "w2", kind: "wing", name: "Wing 2" },
];
const tree = {
  byId: new Map(units.map((u) => [u.id, u])),
  kids: (id) => units.filter((u) => u.parent === id),
  unitOf: (m) => ({ m1: "f1" })[m] || null,
};
let n = 0;
const nextId = () => `n${++n}`;
const used = (layout) => { const l = JSON.parse(JSON.stringify(layout)); UG.fillDefaults(l.objects, tree, { nextId }); return l; };
const binds = (list) => list.map((o) => o.bind);

test("a v3 round trip keeps ids, positions, binds, looks, labels, children and the cell size only", () => {
  const out = UG.serializeLayout({ cell: 64, objects: [
    obj("a", "unit", { bind: "w", look: "condensed", w: 24, h: 9, card: { x: 1 }, isNew: true, children: [
      obj("b", "unit", { bind: "s1", w: 12, h: 4, label: "<b>Lead", children: [obj("c", "member", { bind: "m1", look: "full", x: 10, w: 6, fields: {} })] }),
      obj("d", "unit", { bind: "s2", x: 30, w: 40, children: [] }),
    ] }),
    obj("e", "timeline", { y: 9, children: [] }),
  ] });
  assert.deepEqual(out, { schemaVersion: 3, columns: 24, cell: 64, rows: 12, objects: [
    { id: "a", kind: "unit", x: 0, y: 0, w: 24, h: 9, bind: "w", look: "condensed", children: [
      { id: "b", kind: "unit", x: 0, y: 0, w: 12, h: 4, bind: "s1", label: "bLead", children: [{ id: "c", kind: "member", x: 6, y: 0, w: 6, h: 3, bind: "m1", look: "full" }] },
      { id: "d", kind: "unit", x: 0, y: 0, w: 24, h: 3, bind: "s2", children: [] },
    ] },
    { id: "e", kind: "timeline", x: 0, y: 9, w: 6, h: 3 },
  ] }, "a child is kept on its unit's own columns; an emptied grid stays empty");
  assert.deepEqual(UG.layoutProblems(out), []);
  assert.deepEqual(UG.serializeLayout(JSON.parse(JSON.stringify(out))), out, "stable");
  assert.equal(UG.serializeLayout({ cell: 70, objects: [] }).cell, 72, "an unknown cell size is 72 px");
  assert.equal(UG.serializeLayout({ cell: 80, objects: [] }).cell, 80, "a stored cell size is kept");
});

test("versions 1 and 2 migrate to 72 px cells and their units' own columns", () => {
  const v1 = { schemaVersion: 1, columns: 24, rows: 3, objects: [obj("a", "unit", { bind: "w", y: 5, h: 4 })] };
  const m = UG.migrateLayout(v1);
  assert.deepEqual([m.schemaVersion, m.cell, m.objects[0].y, m.objects[0].h, m.objects[0].w], [3, 72, 4, 3, 6], "54 px rows in 72 px cells");
  assert.equal(v1.schemaVersion, 1, "the input is left as it came");
  assert.deepEqual(UG.layoutProblems(m), []);
  assert.deepEqual(UG.layoutProblems(v1), ["unknown schema version"]);
  const v2 = { schemaVersion: 2, columns: 24, rows: 9, objects: [obj("w", "unit", { bind: "w", w: 12, h: 8, children: [obj("s", "ship", { bind: "ship:f1", x: 12, w: 12, h: 1 }), obj("m", "member", { x: 23, w: 1 })] })] };
  const m2 = UG.migrateLayout(v2);
  assert.deepEqual(m2.objects[0].children.map((o) => [o.x, o.w, o.h]), [[6, 6, 1], [11, 1, 2]], "as layouts.rs upgrades");
  assert.deepEqual(UG.layoutProblems(m2), []);
  assert.equal(UG.migrateLayout(m2), m2, "once");
  assert.equal(UG.migrateLayout(null), null);
  const auto = used(UG.serializeLayout(UG.migrateLayout({ schemaVersion: 2, columns: 24, objects: [obj("a", "unit", { bind: "f", w: 24 })] })));
  assert.deepEqual(binds(auto.objects[0].children), ["i1", "i2"], "a v2 auto list loads as its default grid");
});

test("the checker refuses depth, the cap, kinds in a unit, ids, a child past its unit's width and odd cells", () => {
  const p = (objects, over) => UG.layoutProblems(Object.assign(v3(objects), over || {})).join("; ");
  const deep = obj("a", "unit", { w: 24, children: [obj("b", "unit", { w: 24, children: [obj("c", "unit", { w: 24, children: [obj("d", "unit", { children: [obj("e", "member")] })] })] })] });
  assert.match(p([deep]), /nest at most 3 deep/);
  const ok3 = obj("a", "unit", { w: 24, children: [obj("b", "unit", { w: 24, children: [obj("c", "unit", { w: 24, children: [obj("d", "member")] })] })] });
  assert.equal(p([ok3]), "", "a grid at depth 2 with cards at depth 3 is fine");
  const many = Array.from({ length: 60 }, (_, i) => obj(`t${i}`, "rollup"));
  const box = obj("box", "unit", { w: 24, children: Array.from({ length: 40 }, (_, i) => obj(`c${i}`, "member")) });
  assert.match(p([...many, box]), /more than 100 objects/, "counted across all levels");
  assert.equal(p([...many.slice(1), box]), "");
  for (const kind of ["header", "unassigned", "timeline"]) assert.match(p([obj("a", "unit", { children: [obj("b", kind)] })]), /stays on the board/, kind);
  assert.match(p([obj("a", "rollup", { children: [] })]), /only a unit holds/);
  assert.match(p([obj("a", "unit", { w: 24, children: [obj("b", "unit", { children: [obj("a", "member")] })] })]), /unique/);
  assert.match(p([obj("a", "unit", { w: 10, children: [obj("b", "member", { x: 6, w: 6 })] })]), /outside the grid/, "a unit's grid is as wide as the unit");
  assert.equal(p([obj("a", "unit", { w: 12, children: [obj("b", "member", { x: 6, w: 6 })] })]), "");
  assert.match(p([obj("a", "unit", { children: [obj("b", "member", { bind: "<x>" })] })]), /binds are ids/);
  assert.match(p([], { cell: 70 }), /cells are 48, 64, 72, 80, 96 px/);
});

test("the serialiser drops what the checker refuses and caps across levels", () => {
  const out = UG.serializeLayout({ objects: [obj("a", "unit", { w: 24, children: [obj("h", "header"), obj("b", "unit", { w: 24, children: [obj("c", "unit", { w: 24, children: [obj("d", "unit", { children: [obj("e", "member")] })] })] })] })] });
  const ids = [];
  UG.walk(out.objects, (o) => ids.push(o.id));
  assert.deepEqual(ids, ["a", "b", "c", "d"], "no header inside, nothing below depth 3");
  assert.deepEqual(UG.layoutProblems(out), []);
  const big = UG.serializeLayout({ objects: [obj("box", "unit", { children: Array.from({ length: 130 }, (_, i) => obj(`c${i}`, "member")) })] });
  assert.equal(UG.count(big.objects), UG.MAX_OBJECTS);
});

test("default grids per unit kind: a fleet's ships, a wing's squadrons and their ships, a ground force's squads", () => {
  const l = used(v3([obj("F", "unit", { bind: "f", w: 24 }), obj("W", "unit", { bind: "w", w: 24, look: "condensed" }), obj("G", "unit", { bind: "g", w: 12 }), obj("I", "unit", { bind: "w2", w: 6 })]));
  const [fleet, wing, ground, empty] = l.objects;
  assert.deepEqual(fleet.children.map((o) => [o.bind, o.x, o.y, o.w]), [["i1", 0, 0, 12], ["i2", 12, 0, 12]], "two ships side by side");
  assert.deepEqual(fleet.children.map((o) => o.children), [[], []], "a ship is the leaf: an empty grid");
  assert.deepEqual(wing.children.map((o) => [o.bind, o.look]), [["s1", "condensed"], ["s2", "condensed"]], "the look carries down");
  assert.deepEqual(wing.children[0].children.map((o) => [o.bind, o.x, o.w]), [["f1", 0, 6], ["f2", 6, 6]], "recursively, on the squadron's own 12 columns");
  assert.deepEqual(binds(ground.children), ["q1"]);
  assert.deepEqual(empty.children, []);
  UG.walk(l.objects, (o, depth) => { if (o.kind === "unit" && depth <= UG.MAX_CONTAINER_DEPTH) assert.ok(o.auto, o.id); });
  assert.deepEqual(UG.serializeLayout(l).objects.map((o) => o.children), [undefined, undefined, undefined, undefined], "untouched defaults are not saved");

  const placed = used(v3([obj("F", "unit", { bind: "f", w: 24 }), obj("X", "unit", { bind: "i2", y: 5 })]));
  assert.deepEqual(binds(placed.objects[0].children), ["i1"], "a unit placed elsewhere is not repeated");
  const filler = Array.from({ length: 99 }, (_, i) => obj(`t${i}`, "rollup"));
  const capped = used(v3([obj("F", "unit", { bind: "f", w: 24 }), ...filler]));
  assert.equal(UG.count(capped.objects), 100, "the cap holds");
  const deep = used(v3([obj("a", "unit", { bind: "zz", w: 24, children: [obj("b", "unit", { bind: "zz", w: 24, children: [obj("c", "unit", { bind: "w", w: 24, children: [obj("d", "unit", { bind: "s1" })] })] })] })]));
  assert.equal(UG.find(deep.objects, "d").object.children, undefined, "no grid past depth 2");
});

test("add inside a unit: the palette offers only what the unit may hold", () => {
  const l = used(v3([obj("F", "unit", { bind: "f", w: 24 }), obj("W", "unit", { bind: "w", w: 24, y: 6 })]));
  const ship = l.objects[0].children[0];
  const kinds = (id) => UG.holds(UG.targetOf(l.objects, id), tree);
  assert.deepEqual(kinds(null), Object.keys(UG.KINDS), "the board takes every kind");
  assert.deepEqual(kinds("F"), ["unit", "member", "ship", "rollup", "alerts"]);
  assert.deepEqual(kinds(ship.id), ["member", "ship", "rollup", "alerts"], "a ship has no child units");
  const sqShip = l.objects[1].children[0].children[0];
  assert.deepEqual(kinds(sqShip.id), ["member", "ship", "rollup", "alerts"], "a ship at depth 2 still takes cards");
  assert.deepEqual(UG.targetOf(l.objects, "nope"), { object: null, depth: -1 });

  const added = UG.addTo(l, "F", obj("m", "member", { bind: "m1", w: 30 }));
  const fleet = added.objects[0];
  assert.deepEqual([fleet.children.at(-1).id, fleet.children.at(-1).place, fleet.children.at(-1).w, fleet.touched], ["m", true, 24, true]);
  assert.equal(l.objects[0].children.length, 2, "the input is left as it came");
  assert.deepEqual(UG.targetOf(added.objects, "m").object.id, "F", "a card selected adds next to itself");
  const saved = UG.serializeLayout(added);
  assert.deepEqual(saved.objects[0].children.map((o) => o.kind), ["unit", "unit", "member"], "an edited grid is saved");
  assert.equal(saved.objects[0].children[0].children, undefined, "its untouched ships are left to their defaults");
  const inShip = UG.addTo(added, ship.id, obj("m2", "member"));
  assert.equal(UG.find(inShip.objects, "m2").parent.id, ship.id);
  const sq = UG.addTo(l, sqShip.id, obj("u", "unit", { children: [obj("x", "member")] }));
  assert.equal(sq, null, "too deep");
  assert.equal(UG.addTo(l, "F", obj("h", "header")), null, "a header stays on the board");
  const board = UG.addTo(l, null, obj("t", "timeline", { w: 40 }));
  assert.equal(board.objects.at(-1).w, 24);
  const full = v3(Array.from({ length: 100 }, (_, i) => obj(`t${i}`, "rollup")));
  assert.equal(UG.addTo(full, null, obj("x", "rollup")), null, "the cap");
});

test("move, resize and remove inside units mark the grid as edited", () => {
  const l = used(v3([obj("W", "unit", { bind: "w", w: 24 })]));
  const [s1] = l.objects[0].children;
  UG.touchParent(l.objects, s1.children[0].id);
  const kept = UG.serializeLayout(l);
  assert.ok(Array.isArray(kept.objects[0].children), "a grid edited deep down is saved with the grids around it");
  assert.deepEqual(binds(kept.objects[0].children[0].children), ["f1", "f2"]);
  assert.equal(kept.objects[0].children[1].children, undefined, "its untouched sibling is not");

  const fresh = used(v3([obj("W", "unit", { bind: "w", w: 24 })]));
  const gone = UG.removeFrom(fresh, fresh.objects[0].children[1].id);
  assert.deepEqual(binds(UG.serializeLayout(gone).objects[0].children), ["s1"], "a removed default stays removed");
  const emptied = UG.removeFrom(UG.removeFrom(fresh, fresh.objects[0].children[0].id), fresh.objects[0].children[1].id);
  assert.deepEqual(UG.serializeLayout(emptied).objects[0].children, [], "an emptied grid is saved empty");
  assert.deepEqual(used(UG.serializeLayout(emptied)).objects[0].children, [], "and loads empty, not as the default");
  const reset = UG.resetInside(emptied, "W");
  assert.deepEqual(binds(used(reset).objects[0].children), ["s1", "s2"], "default cards inside again");
  UG.touch(fresh.objects, "W");
  assert.ok(Array.isArray(UG.serializeLayout(fresh).objects[0].children));
});

test("a child unit with no object is appended: into a default grid as part of it, elsewhere marked new", () => {
  const layout = UG.serializeLayout({ objects: [obj("a", "unit", { bind: "w", look: "condensed", w: 24, h: 8, children: [obj("b", "unit", { bind: "s1", w: 12, h: 5 })] })] });
  const box = layout.objects[0];
  const added = UG.autoAppend(box, layout, tree, nextId);
  assert.deepEqual(added.map((o) => [o.bind, o.isNew, o.look, o.y]), [["s2", true, "condensed", 5]]);
  assert.equal(UG.autoAppend(box, layout, tree, nextId).length, 0, "only once");
  assert.deepEqual(binds(UG.serializeLayout({ objects: UG.settleNew(layout.objects, false) }).objects[0].children), ["s1"], "not saved unless kept");
  const kept = UG.serializeLayout({ objects: UG.settleNew(layout.objects, true) });
  assert.deepEqual(binds(kept.objects[0].children), ["s1", "s2"]);
  assert.ok(!JSON.stringify(kept).includes("isNew"));
  const auto = used(v3([obj("F", "unit", { bind: "f", w: 24 })]));
  auto.objects[0].children.pop();
  const joined = UG.autoAppend(auto.objects[0], auto, tree, nextId);
  assert.deepEqual(joined.map((o) => [o.bind, o.isNew]), [["i2", undefined]], "a default grid takes a new ship as its own");
  const elsewhere = UG.serializeLayout({ objects: [obj("a", "unit", { bind: "w", w: 24, children: [obj("b", "unit", { bind: "s1" })] }), obj("c", "unit", { bind: "s2", y: 5 })] });
  assert.equal(UG.autoAppend(elsewhere.objects[0], elsewhere, tree, nextId).length, 0, "a unit placed on the board stays there");
});

test("the nested unplaced walk covers placed units, their subtrees and their ancestors", () => {
  const layout = v3([obj("a", "unit", { bind: "w2" }), obj("r", "rollup", { bind: "s2" }), obj("b", "rollup"),
    obj("x", "unit", { bind: "zz", children: [obj("y", "unit", { bind: "f1" })] }), obj("F", "unit", { bind: "f" }), obj("G", "unit", { bind: "g" })]);
  assert.deepEqual(UG.unplacedUnits(layout, tree).map((u) => u.id), ["s2", "f2"], "a placed ship covers its squadron, not its sibling");
});

test("move out to the board and put back in its unit", () => {
  const layout = UG.serializeLayout({ objects: [obj("a", "unit", { bind: "w", w: 24, h: 8, children: [obj("b", "unit", { bind: "s1", w: 12, h: 4, children: [obj("c", "member", { bind: "m1" }), obj("c2", "member", { x: 6, bind: "m2" })] }), obj("d", "unit", { bind: "s2", x: 12, w: 12, h: 4 })] })] });
  const out = UG.moveOut(layout, "d");
  assert.deepEqual(out.objects.map((o) => o.id), ["a", "d"]);
  assert.equal(out.objects[1].y, 8, "at the bottom of the board");
  assert.ok(out.objects[0].touched);
  assert.equal(UG.moveOut(out, "d"), null, "already on the board");
  const back = UG.putBack(out, "d", tree);
  assert.deepEqual(back.objects[0].children.map((o) => o.id), ["b", "d"]);
  assert.equal(UG.find(back.objects, "d").object.y, 4);
  assert.equal(UG.putBack(back, "d", tree), null, "already home");
  const card = UG.putBack(UG.moveOut(layout, "c"), "c", tree);
  assert.equal(UG.find(card.objects, "c").parent.id, "b", "a member goes back to the nearest grid of their unit");
  assert.equal(UG.putBack(layout, "a", tree), null, "a top unit has no home");
  const narrow = UG.putBack(UG.moveOut(layout, "c"), "c", tree);
  assert.ok(UG.find(narrow.objects, "c").object.w <= 12, "narrowed to fit its unit");
  const emptied = UG.moveOut(UG.moveOut(layout, "c"), "c2");
  assert.deepEqual(UG.find(emptied.objects, "b").object.children, [], "a grid left empty stays an empty grid");
  assert.deepEqual(layout.objects[0].children.map((o) => o.id), ["b", "d"], "the input is left as it came");
});

test("ours to gridstack and back: sub-grids as wide in columns as their unit, the size is the user's", () => {
  const layout = UG.serializeLayout({ objects: [obj("a", "unit", { bind: "w", w: 16, h: 8, children: [obj("b", "member", { bind: "m1" })] }), obj("t", "timeline", { y: 8 }), obj("e", "unit", { y: 12, children: [] })] });
  layout.objects[1].place = true;
  const gs = UG.toGridstack(layout.objects, { margin: 4 });
  assert.deepEqual(gs[0], { id: "a", x: 0, y: 0, w: 16, h: 8, minW: 3, minH: 1, subGridOpts: { margin: 4, column: 16, children: [{ id: "b", x: 0, y: 0, w: 6, h: 3, minW: 2, minH: 1 }] } });
  assert.equal(gs[1].autoPosition, true);
  assert.deepEqual([gs[2].sizeToContent, gs[2].subGridOpts], [undefined, undefined], "an empty grid has no sub-grid");
  const moved = UG.withPositions(layout.objects, [{ id: "b", x: 6, y: 1, w: 8, h: 4 }, { id: "a", x: 0, y: 2, w: 24, h: 9 }]);
  assert.deepEqual([moved[0].y, moved[0].children[0].x, moved[0].children[0].w], [2, 6, 8]);
  assert.equal(layout.objects[0].children[0].x, 0, "the input is left as it came");
  const narrowed = UG.clampTo([obj("a", "unit", { w: 6, children: [obj("b", "member", { x: 4, w: 8 })] })], 24);
  assert.deepEqual([narrowed[0].children[0].x, narrowed[0].children[0].w], [0, 6], "a narrower unit narrows what it holds");
});

test("a layouts.json saved by the version 2 build (own and event default) migrates and fills", () => {
  const saved = JSON.parse(require("node:fs").readFileSync(require("node:path").join(__dirname, "../fixtures/layouts-v2-saved.json"), "utf8"));
  const real = [
    { id: "u1", kind: "fleet", name: "Fleet" }, { id: "u2", kind: "wing", name: "Wing" }, { id: "u3", kind: "ground", name: "Ground" },
    { id: "u4", kind: "ship", name: "Ship", parent: "u1" }, { id: "u5", kind: "squadron", name: "Squadron", parent: "u2" },
  ];
  const t = { byId: new Map(real.map((u) => [u.id, u])), kids: (id) => real.filter((u) => u.parent === id) };
  assert.equal(saved.layouts.length, 2);
  for (const { layout } of saved.layouts) {
    assert.equal(layout.schemaVersion, 2);
    const l = UG.serializeLayout(UG.migrateLayout(layout));
    assert.deepEqual(UG.layoutProblems(l), []);
    assert.deepEqual(l.objects.map((o) => o.id), ["o1", "o2", "o3", "o4", "o5", "o6", "o7", "o8", "o9"]);
    assert.deepEqual(l.objects.filter((o) => o.kind === "unit").map((o) => [o.look, o.y, o.h]), [["condensed", 2, 8], ["cards", 10, 8], ["condensed", 2, 8]]);
    assert.equal(l.objects[1].label, "Event");
    UG.fillDefaults(l.objects, t, { nextId });
    UG.clampTo(l.objects, UG.COLUMNS);
    const widgets = UG.toGridstack(l.objects, {});
    assert.equal(widgets.length, 9);
    assert.deepEqual(widgets.filter((w) => w.subGridOpts).map((w) => [w.id, w.subGridOpts.children.map((c) => c.id).length]), [["o7", 1]], "the fleet's ship is bound on the board, so only the wing's squadron fills in");
    assert.deepEqual(UG.serializeLayout(l).objects.map((o) => Boolean(o.children)), Array(9).fill(false), "default grids are not saved");
  }
});

test("a unit is at least as tall as its header and people, plus a grid strip when it holds objects", () => {
  assert.equal(UG.unitMinRows(0, false, 80), 1);
  assert.equal(UG.unitMinRows(76, false, 80), 1, "76 px and the item's inset and border, 4 px, fit one 80 px cell");
  assert.equal(UG.unitMinRows(77, false, 80), 2);
  assert.equal(UG.unitMinRows(70, true, 80), 2, "a strip of the grid stays in view");
  assert.equal(UG.unitMinRows(300, true, 48), 7);
  assert.equal(UG.unitMinRows(1e6, false, 80), 99);
  assert.equal(UG.toGridstack([obj("u", "unit", { bind: "w", children: [obj("c", "member")] })])[0].sizeToContent, undefined, "the size is the user's, not the content's");
});

// Stand-in heights of a unit's header and own people: a ship two cells, a squad three, the rest one.
const seen = [];
const detailsPx = (u, px, look, holds) => { seen.push([u.id, px, look, holds]); return ({ ship: 134, squad: 206 })[u.kind] || 62; };
const natural = (id, over) => UG.naturalRows(id, tree, Object.assign({ cols: 24, cell: 72, detailsPx }, over));
const filled = (layout) => { UG.fillDefaults(layout.objects, tree, { nextId, cell: 72, detailsPx }); return layout; };

test("a unit's natural height: its own people, then its child units side by side, the tallest of each row", () => {
  assert.equal(natural("i1"), 2, "a ship: its people only");
  assert.equal(natural("f"), 3, "a fleet: one row of its own, then its two ships side by side");
  assert.equal(natural("w"), 4, "a wing: its squadrons side by side, the taller holding two ships");
  assert.equal(natural("g"), 4, "a ground force over its squad");
  assert.equal(natural("w2"), 1, "a wing with no squadrons");
  assert.equal(natural("f", { cols: 4 }), 5, "one ship across in a narrow fleet: the ships stack");
  assert.equal(natural("f", { cell: 48 }), 5, "at 48 px cells");
  seen.length = 0;
  assert.equal(natural("w", { depth: 2, look: "condensed" }), 2, "a wing two deep: its squadrons hold no grid");
  assert.deepEqual(seen.map(([id, px, look, holds]) => [id, px, look, holds]), [["w", 24 * 72, "condensed", true], ["s1", 12 * 72, "condensed", false], ["s2", 12 * 72, "condensed", false]]);
  assert.equal(natural("s1", { skip: new Set(["f1", "f2"]) }), 1, "units shown elsewhere are left out");
  assert.equal(natural("nope"), 1);
});

test("default grids take their natural heights; a grid reset or a unit just added grows to show it, a saved size stays", () => {
  const l = filled(v3([obj("W", "unit", { bind: "w", w: 24, h: 1 }), obj("G", "rollup", { y: 1 })]));
  const [W] = l.objects;
  assert.equal(W.h, 1, "a saved size stays as saved");
  assert.deepEqual(W.children.map((o) => [o.bind, o.x, o.y, o.w, o.h]), [["s1", 0, 0, 12, 3], ["s2", 12, 0, 12, 1]]);
  assert.deepEqual(W.children[0].children.map((o) => [o.bind, o.h]), [["f1", 2], ["f2", 2]]);

  const reset = filled(UG.resetInside(UG.serializeLayout(l), "W"));
  assert.deepEqual(reset.objects.map((o) => [o.id, o.y, o.h]), [["W", 0, 4], ["G", 4, 3]], "it grows and what sits below it moves down");
  const gap = filled(UG.resetInside(UG.serializeLayout(filled(v3([obj("W", "unit", { bind: "w", w: 24, h: 1 }), obj("G", "rollup", { y: 6 })]))), "W"));
  assert.deepEqual(gap.objects.map((o) => [o.id, o.y, o.h]), [["W", 0, 4], ["G", 6, 3]], "what is clear of it stays put");
  assert.ok(!("fit" in reset.objects[0]) && !JSON.stringify(UG.serializeLayout(reset)).includes("fit"));

  const nested = v3([obj("W", "unit", { bind: "w", w: 24, h: 2, children: [obj("S", "unit", { bind: "s1", w: 12, h: 1, children: [] }), obj("R", "rollup", { y: 1 })] })]);
  const grown = filled(UG.resetInside(nested, "S"));
  const [outer] = grown.objects;
  assert.deepEqual(outer.children.map((o) => [o.id, o.y, o.h]), [["S", 0, 3], ["R", 3, 3]]);
  assert.equal(outer.h, 7, "the grid it sits in grows to hold it: one row of its own and six of grid");

  const added = filled(UG.addTo(v3([obj("T", "timeline", { w: 24 })]), null, obj("u", "unit", { bind: "g", w: 12, h: 1 })));
  assert.deepEqual(added.objects.map((o) => [o.id, o.y, o.h]), [["T", 0, 3], ["u", 0, 4]], "a unit just added takes its natural height and waits for its spot");
  const inside = filled(UG.addTo(v3([obj("W", "unit", { bind: "w", w: 24, h: 3, children: [] })]), "W", obj("u", "unit", { bind: "s1", w: 12, h: 1 })));
  assert.equal(UG.find(inside.objects, "u").object.h, 3);
  assert.equal(inside.objects[0].h, 3, "one waiting for its spot leaves its grid's size alone");
});

test("units appended to a grid take the height they are given, the tallest setting each row", () => {
  const layout = UG.serializeLayout({ objects: [obj("a", "unit", { bind: "w", w: 24, h: 8, children: [] })] });
  layout.objects[0].auto = true;
  const added = UG.autoAppend(layout.objects[0], layout, tree, nextId, (u, w) => natural(u.id, { cols: w, depth: 1 }));
  assert.deepEqual(added.map((o) => [o.bind, o.x, o.y, o.w, o.h]), [["s1", 0, 0, 12, 3], ["s2", 12, 0, 12, 1]]);
  const narrow = UG.serializeLayout({ objects: [obj("a", "unit", { bind: "w", w: 4, h: 8, children: [] })] });
  narrow.objects[0].auto = true;
  const stacked = UG.autoAppend(narrow.objects[0], narrow, tree, nextId, (u, w) => natural(u.id, { cols: w, depth: 1 }));
  assert.deepEqual(stacked.map((o) => [o.bind, o.y, o.h]), [["s1", 0, 5], ["s2", 5, 1]], "one across: the ships under s1 stack too");
});

test("a kind added from the palette starts tall enough for its content at 72 px cells", () => {
  // Measured in a browser with the mock feed: a roll-up needs 2 rows and a ship card 3.
  const rows = Object.fromEntries(Object.entries(UG.KINDS).map(([k, v]) => [k, v.h]));
  assert.deepEqual(rows, { header: 1, unit: 4, member: 3, ship: 3, rollup: 2, alerts: 3, unassigned: 3, timeline: 3 });
  for (const [k, v] of Object.entries(UG.KINDS)) assert.ok(v.h >= v.minH && v.w >= v.minW, `${k} starts within its minimum`);
});

test("units sized by default settle to the rows they are drawn in, once, and leave edited sizes alone", () => {
  // Drawn heights: a ship's own people two cells less a little, the rest a header only.
  const own = (o) => ({ i1: 120, i2: 60, f: 30 })[o.bind] ?? 30;
  const layout = () => [obj("H", "header", { w: 24, h: 1 }),
    obj("F", "unit", { bind: "f", y: 1, w: 24, h: 9, auto: true, children: [obj("A", "unit", { bind: "i1", w: 12, h: 5 }), obj("B", "unit", { bind: "i2", x: 12, w: 12, h: 5 })] }),
    obj("R", "rollup", { y: 10, w: 6, h: 2 })];
  const l = layout();
  assert.equal(UG.settleRows(l, own, 72, true), true);
  const [a, b] = l[1].children;
  assert.deepEqual([a.h, b.h, a.y, b.y], [2, 1, 0, 0], "each child as tall as it is drawn, side by side");
  assert.equal(l[1].h, 1 + 2, "the fleet: its header, then its grid's rows");
  assert.equal(l[2].y, l[1].y + l[1].h, "what sat below moves up to close the gap");
  assert.equal(UG.settleRows(l, own, 72, true), false, "a second pass moves nothing");
  const kept = layout();
  kept[1].touched = true;
  UG.settleRows(kept, own, 72, false);
  assert.deepEqual([kept[1].h, kept[1].children[0].h, kept[2].y], [9, 5, 10], "an edited grid and saved sizes stay");
});

test("a unit grown to show a reset grid settles to what it shows, and what it pushed down comes back up", () => {
  const own = () => 30;
  const l = [obj("W", "unit", { bind: "w", w: 24, h: 9, settle: true, touched: true, children: [obj("S", "unit", { bind: "s1", w: 12, h: 3 })] }),
    obj("R", "rollup", { y: 9, w: 6, h: 2 }), obj("T", "timeline", { x: 6, y: 12, w: 18, h: 3 })];
  assert.equal(UG.settleRows(l, own, 72, false), true);
  assert.deepEqual([l[0].h, l[1].y, l[2].y], [4, 4, 7], "four rows; the roll-up and timeline move up five");
  assert.equal(l[0].settle, undefined, "the mark is spent");
  assert.equal(UG.settleRows(l, own, 72, false), false);
});

test("each object's title alignment is its own; unsaid, a unit on the board is centered and the rest left", () => {
  const l = UG.serializeLayout(v3([obj("W", "unit", { bind: "w", w: 24, align: "left", children: [obj("S", "unit", { bind: "s1", w: 12, align: "center" }), obj("T", "unit", { bind: "s2", x: 12, w: 12 })] }), obj("R", "rollup", { y: 3, align: "middle" })]));
  const [W, R] = l.objects;
  const [S, T] = W.children;
  assert.deepEqual([W.align, S.align, T.align, R.align], ["left", "center", undefined, undefined], "set per object; an unknown one is dropped");
  assert.deepEqual(UG.layoutProblems(l), []);
  assert.deepEqual([UG.titleAlign(W, 0), UG.titleAlign(S, 1), UG.titleAlign(T, 1)], ["left", "center", "left"], "a wing set left leaves its squadrons as they are");
  assert.deepEqual([UG.titleAlign(obj("U", "unit"), 0), UG.titleAlign(obj("U", "unit"), 1), UG.titleAlign(obj("R", "rollup"), 0)], ["center", "left", "left"]);
  assert.deepEqual(UG.layoutProblems(v3([obj("A", "unit", { align: "middle" })])), ["title alignments are center or left"]);
  assert.equal("titles" in l, false, "no layout-wide setting is saved");
});

test("a layout-wide titles setting moves onto each unit once, so it looks as it did", () => {
  const old = Object.assign(v3([obj("W", "unit", { bind: "w", w: 24, children: [obj("S", "unit", { bind: "s1", w: 12, align: "left" }), obj("R", "rollup", { x: 12 })] })]), { titles: "center" });
  const l = UG.migrateLayout(old);
  assert.equal("titles" in l, false);
  const [W] = l.objects;
  assert.deepEqual([W.align, W.children[0].align, W.children[1].align], ["center", "left", undefined], "a unit's own stays; other objects stay left");
  assert.equal(old.objects[0].align, undefined, "the saved layout is not changed in place");
  assert.equal(UG.migrateLayout(l), l, "nothing to move: the same layout back");
  assert.deepEqual(UG.layoutProblems(Object.assign(v3([]), { titles: "middle" })), ["titles are center or left"], "the old field is still read");
  const left = UG.migrateLayout(Object.assign(v3([obj("W", "unit", { bind: "w", w: 24 })]), { titles: "left" }));
  assert.equal(UG.titleAlign(left.objects[0], 0), "left", "a top-level unit set left by the old field stays left");
});

test("unit titles step down by tier: the top, its groups, then the units inside them", () => {
  const tier = (depth, id) => UG.titleTier(depth, tree.byId.get(id), tree);
  assert.deepEqual([tier(0, "w"), tier(1, "s1"), tier(2, "f1")], [0, 1, 2], "wing > squadron > ship");
  assert.deepEqual([tier(0, "f"), tier(1, "i1")], [0, 1], "fleet > ship");
  assert.deepEqual([tier(0, "g"), tier(1, "q1")], [0, 1], "ground > squad");
  assert.equal(tier(0, "s1"), 1, "a squadron on its own board is still a group");
  assert.equal(tier(1, "f1"), 2, "a fighter moved out to its wing's grid is still a child");
  assert.equal(tier(0, "f1"), 2, "a fighter on the board is still a child");
  assert.equal(tier(0, "w"), 0);
  assert.equal(UG.titleTier(3, tree.byId.get("w"), tree), 2, "the deepest grid caps at the third tier");
  const alone = { byId: new Map([["x", { id: "x", kind: "squad", parent: "hidden" }]]) };
  assert.equal(UG.titleTier(0, alone.byId.get("x"), alone), 1, "a unit whose parent is out of view goes by its kind");
  assert.equal(UG.titleTier(1, null, tree), 1, "an unknown unit goes by where it sits");
});

test("a unit's least size is its measured header and rows: a fighter's header and one row fit one 72 px cell", () => {
  assert.equal(UG.unitMinRows(20 + 40, false, 72), 1, "a header and a row wrapped to two lines");
  assert.equal(UG.unitMinRows(20 + 40, false, 48), 2, "at 48 px cells it takes two");
  assert.equal(UG.unitMinRows(21 + 24, true, 72), 2, "a squadron's header and lead, then a strip of its grid");
  assert.equal(UG.unitMinRows(21 + 24 + 7 * 20, false, 72), 3, "an Idris: its ship card and six crew rows");
});
