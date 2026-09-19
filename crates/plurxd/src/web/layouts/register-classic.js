"use strict";
LAYOUTS.classic.chrome=classicChrome;
LAYOUTS.classic.views={
  home:classicHomeBody,
  item:classicItemBody,
  // Region-shaped rather than a single function: library is the app's one
  // incremental route (§3.2) and its regions are redrawn independently.
  library:{shell:classicLibraryShell, items:classicLibraryItems, count:classicLibraryCount},
};
