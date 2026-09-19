"use strict";

// What the web assets may name at load, and when.
//
// The split shell (docs/clients/WEB-SHELL-LAYOUT.md) is sixty-two plain
// scripts sharing one global scope. Hoisting is per script, so a file may only
// name a binding declared in an *earlier* file at the moment it loads — and the
// thirty-nine statements that run at load are the only ones that care. Get the
// order wrong and the app dies on a blank page with a ReferenceError.
//
// A runtime load does not catch this: swapping each adjacent pair of the sixty
// files and loading them in a stub catches 4 of 59 swaps, because almost
// everything in the shell is a function declaration that nobody calls until a
// route renders. So this reads the code instead — a real parse, not a regex
// over 1.3 MB — and answers three questions:
//
//   * does any load-time expression name a binding declared in a later row,
//     directly or through a function it calls,
//   * is any top-level name declared in two rows,
//   * does any row lack its `"use strict";` prologue.
//
// Callbacks handed to `addEventListener`, `setTimeout` and friends are not
// load-time: they run when the event does, by which point every row is in.

const acorn = require("../vendor/acorn.js");
const {shellSource} = require("./shell-source.js");

// A function argument to one of these runs later, not now.
const DEFERRED_SINKS = new Set([
  "addEventListener", "removeEventListener", "setTimeout", "setInterval",
  "requestAnimationFrame", "requestIdleCallback", "queueMicrotask",
  "then", "catch", "finally", "observe", "subscribe",
]);

const PARSE = {ecmaVersion: "latest", sourceType: "script", locations: true};

function boundNames(pattern, into) {
  if (!pattern) return;
  switch (pattern.type) {
    case "Identifier": into.add(pattern.name); break;
    case "ObjectPattern":
      for (const prop of pattern.properties) {
        boundNames(prop.type === "RestElement" ? prop.argument : prop.value, into);
      }
      break;
    case "ArrayPattern":
      for (const el of pattern.elements) boundNames(el, into);
      break;
    case "AssignmentPattern": boundNames(pattern.left, into); break;
    case "RestElement": boundNames(pattern.argument, into); break;
    default: break;
  }
}

function topLevelNames(program) {
  const names = new Set();
  for (const node of program.body) {
    if (node.type === "FunctionDeclaration" || node.type === "ClassDeclaration") {
      if (node.id) names.add(node.id.name);
    } else if (node.type === "VariableDeclaration") {
      for (const d of node.declarations) boundNames(d.id, names);
    }
  }
  return names;
}

/** Every child node, so the walk needs no per-type table. */
function children(node) {
  const out = [];
  for (const key of Object.keys(node)) {
    if (key === "type" || key === "start" || key === "end" || key === "loc") continue;
    const value = node[key];
    if (Array.isArray(value)) {
      for (const item of value) if (item && typeof item.type === "string") out.push(item);
    } else if (value && typeof value.type === "string") {
      out.push(value);
    }
  }
  return out;
}

const FUNCTIONISH = new Set([
  "FunctionDeclaration", "FunctionExpression", "ArrowFunctionExpression",
]);

/** Names a function-ish node binds: its parameters, and itself if named. */
function functionScope(node) {
  const names = new Set();
  for (const p of node.params) boundNames(p, names);
  if (node.type === "FunctionExpression" && node.id) names.add(node.id.name);
  // Everything declared anywhere inside the body — `var` is function-scoped and
  // a nested `let` only ever shadows more, so over-collecting here can only
  // make the gate quieter, never wrong about an ordering that really breaks.
  const stack = node.body ? [node.body] : [];
  while (stack.length) {
    const n = stack.pop();
    if (n !== node.body && FUNCTIONISH.has(n.type)) {
      if (n.id) names.add(n.id.name);
      continue;
    }
    if (n.type === "ClassDeclaration" && n.id) names.add(n.id.name);
    if (n.type === "VariableDeclaration") for (const d of n.declarations) boundNames(d.id, names);
    if (n.type === "FunctionDeclaration" && n.id) names.add(n.id.name);
    if (n.type === "CatchClause") boundNames(n.param, names);
    for (const c of children(n)) stack.push(c);
  }
  return names;
}

/** The identifier positions that are not references to a binding. */
function skipChild(node, child) {
  if (node.type === "MemberExpression" && !node.computed && child === node.property) return true;
  if (node.type === "Property" && !node.computed && child === node.key) return true;
  if (node.type === "MethodDefinition" && !node.computed && child === node.key) return true;
  if (node.type === "PropertyDefinition" && !node.computed && child === node.key) return true;
  if (node.type === "LabeledStatement" && child === node.label) return true;
  if ((node.type === "BreakStatement" || node.type === "ContinueStatement")
      && child === node.label) return true;
  return false;
}

function deferredArguments(node) {
  if (node.type !== "CallExpression") return new Set();
  const callee = node.callee;
  const name = callee.type === "Identifier" ? callee.name
    : callee.type === "MemberExpression" && !callee.computed ? callee.property.name
    : null;
  if (!name || !DEFERRED_SINKS.has(name)) return new Set();
  return new Set(node.arguments.filter((a) => FUNCTIONISH.has(a.type)));
}

/**
 * Free identifiers an expression reaches when evaluated now, following calls to
 * top-level functions. `declaredBy` maps a top-level name to its declaration
 * node so a call can be walked into.
 */
function reachedNames(roots, declaredBy) {
  const found = new Map();            // name -> the call chain that reached it
  const entered = new Set();          // top-level functions already walked
  const work = roots.map((node) => ({node, scopes: [], via: []}));

  while (work.length) {
    const {node, scopes, via} = work.pop();

    // A function expression does not run because it was written down. It runs
    // when something calls it, and by then every row is loaded — the only
    // exception being the one that calls itself on the spot, below.
    if (FUNCTIONISH.has(node.type)) continue;

    if (node.type === "Identifier") {
      if (!scopes.some((s) => s.has(node.name)) && !found.has(node.name)) {
        found.set(node.name, via);
      }
      continue;
    }

    // An IIFE is the one function expression that does run now.
    if (node.type === "CallExpression" && FUNCTIONISH.has(node.callee.type)) {
      const fn = node.callee;
      work.push({node: fn.body, scopes: [...scopes, functionScope(fn)], via});
      for (const argument of node.arguments) work.push({node: argument, scopes, via});
      continue;
    }

    // Only a *call* runs a named function now. `window.startNativeReader =
    // startNativeReader` names one without running it, and what its body
    // reaches is that function's problem at call time, not this row's at load.
    if (node.type === "CallExpression" && node.callee.type === "Identifier") {
      const name = node.callee.name;
      if (!scopes.some((s) => s.has(name))) {
        if (!found.has(name)) found.set(name, via);
        const decl = declaredBy.get(name);
        if (decl && !entered.has(name)) {
          entered.add(name);
          work.push({node: decl.body, scopes: [functionScope(decl)], via: [...via, name]});
        }
      }
      for (const argument of node.arguments) work.push({node: argument, scopes, via});
      continue;
    }

    const deferred = deferredArguments(node);
    for (const c of children(node)) {
      if (skipChild(node, c)) continue;
      if (deferred.has(c)) continue;
      // `X.onclick = function(){}` is a handler, not a call.
      if (node.type === "AssignmentExpression" && c === node.right
          && FUNCTIONISH.has(c.type) && node.left.type === "MemberExpression"
          && !node.left.computed && /^on[a-z]/.test(node.left.property.name || "")) {
        continue;
      }
      work.push({node: c, scopes, via});
    }
  }
  return found;
}

/** The statements and initializers a row evaluates the moment it loads. */
function loadTimeRoots(program) {
  const roots = [];
  for (const node of program.body) {
    if (node.type === "FunctionDeclaration" || node.type === "ClassDeclaration") continue;
    if (node.type === "VariableDeclaration") {
      for (const d of node.declarations) if (d.init) roots.push(d.init);
      continue;
    }
    if (node.type === "ExpressionStatement"
        && node.expression.type === "Literal"
        && node.expression.value === "use strict") continue;
    roots.push(node);
  }
  return roots;
}

/** The head row then the body rows, in served order — what the browser sees. */
function servedSources() {
  const shell = shellSource();
  return [
    {path: shell.rows.head[0], source: shell.headScript},
    ...shell.rows.body.map((path) => ({
      path,
      source: shell.files.find((f) => f.path === path).source,
    })),
  ];
}

/**
 * `sources` defaults to what is actually served. The test passes a rearranged
 * or mutated list to prove this gate can fail — a gate nobody has watched fail
 * is a gate nobody has tested.
 */
function analyze(sources = servedSources()) {
  const rows = sources.map(({path, source}) => {
    let program;
    try {
      program = acorn.parse(source, PARSE);
    } catch (error) {
      throw new Error(`${path} does not parse: ${error.message}`);
    }
    const first = program.body[0];
    const prologue = Boolean(first && first.type === "ExpressionStatement"
      && first.expression.type === "Literal" && first.expression.value === "use strict");
    return {path, source, program, prologue, names: topLevelNames(program)};
  });

  const declaredIn = new Map();
  const duplicates = [];
  const declaredBy = new Map();
  rows.forEach((row, index) => {
    for (const node of row.program.body) {
      if (node.type === "FunctionDeclaration" && node.id) declaredBy.set(node.id.name, node);
    }
    for (const name of row.names) {
      if (declaredIn.has(name)) {
        duplicates.push({name, rows: [rows[declaredIn.get(name)].path, row.path]});
      } else {
        declaredIn.set(name, index);
      }
    }
  });

  const forwardRefs = [];
  rows.forEach((row, index) => {
    const reached = reachedNames(loadTimeRoots(row.program), declaredBy);
    for (const [name, via] of reached) {
      const at = declaredIn.get(name);
      if (at !== undefined && at > index) {
        forwardRefs.push({row: row.path, name, declaredIn: rows[at].path, via});
      }
    }
  });

  return {
    rows,
    duplicates,
    forwardRefs,
    missingPrologue: rows.filter((r) => !r.prologue).map((r) => r.path),
  };
}

module.exports = {analyze, servedSources, reachedNames, topLevelNames, loadTimeRoots, PARSE};
