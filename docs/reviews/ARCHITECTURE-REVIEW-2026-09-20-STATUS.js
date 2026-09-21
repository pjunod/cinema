(function exposeParser(root, factory) {
  "use strict";

  const parser = factory();
  if (typeof module === "object" && module.exports) module.exports = parser;
  root.ArchitectureReviewStatusParser = parser;
})(typeof globalThis === "undefined" ? this : globalThis, function buildParser() {
  "use strict";

  const EXPECTED_COLUMNS = Object.freeze([
    "Id", "Plan", "Executes", "Priority", "Status", "Model", "Session",
    "Branch / PR", "Last update", "Notes",
  ]);

  function splitMarkdownRow(line) {
    const cells = [];
    let cell = "";
    let escaped = false;
    for (const character of line.trim().slice(1, -1)) {
      if (escaped) {
        cell += character;
        escaped = false;
      } else if (character === "\\") {
        escaped = true;
      } else if (character === "|") {
        cells.push(cell.trim());
        cell = "";
      } else {
        cell += character;
      }
    }
    cells.push(cell.trim());
    return cells;
  }

  function parseBoard(markdown) {
    const lines = markdown.split(/\r?\n/);
    const boardHeading = lines.findIndex(
      (line) => line.trim() === "## The board",
    );
    if (boardHeading < 0) {
      throw new Error("The workboard has no ‘The board’ section.");
    }

    const headerIndex = lines.findIndex(
      (line, index) => index > boardHeading && line.startsWith("| Id | Plan |"),
    );
    if (headerIndex < 0) {
      throw new Error("The workboard table header was not found.");
    }

    const columns = splitMarkdownRow(lines[headerIndex]);
    if (columns.join("\u0000") !== EXPECTED_COLUMNS.join("\u0000")) {
      throw new Error(
        "The workboard columns changed; update this view before trusting it.",
      );
    }

    const rows = [];
    for (const line of lines.slice(headerIndex + 2)) {
      if (!line.startsWith("|")) break;
      const values = splitMarkdownRow(line);
      if (values.length !== EXPECTED_COLUMNS.length) {
        throw new Error(
          `Workboard row has ${values.length} cells instead of ` +
          `${EXPECTED_COLUMNS.length}.`,
        );
      }
      rows.push(Object.fromEntries(
        EXPECTED_COLUMNS.map((column, index) => [column, values[index]]),
      ));
    }
    if (!rows.length) throw new Error("The workboard table contains no plans.");
    return rows;
  }

  return Object.freeze({ EXPECTED_COLUMNS, parseBoard, splitMarkdownRow });
});
