"use strict";
// ---- activity page --------------------------------------------------------
// Where the header pill lands: live streams (with a stop for admins),
// per-library scan state, and the Trakt story — refreshed every 3s.
async function viewActivity(generation=++PAGE_RENDER_GENERATION){
  layoutChrome("activity", `<h2 class="section" style="margin-top:6px">Now playing</h2>
    <div class="empty" aria-busy="true">Checking current activity…</div>
    ${typeof ME!=="undefined"&&ME&&ME.is_admin?'<h2 class="section">Content analysis</h2>':""}
    <h2 class="section">Pre-transcoding</h2><h2 class="section">Offline downloads</h2>
    <h2 class="section">Library</h2><h2 class="section">Trakt</h2>`);
  setPagePhase("#/activity",generation,"shell");
  if(ACTIVITY_SNAPSHOT){
    paintActivityBody(ACTIVITY_SNAPSHOT,ACTIVITY_DVR.rows,ACTIVITY_DVR);
    setPagePhase("#/activity",generation,"content");
  }
  await renderActivityBody(generation);
  if(DVR_PAGE.selectedId&&!DVR_PAGE.detailBusy){if(!DVR_PAGE.selected)selectDvrDetail(DVR_PAGE.selectedId);else scheduleDvrHistoryRefresh();}
  if(generation!==PAGE_RENDER_GENERATION||location.hash!=="#/activity") return;
  setPageTimer(()=>renderActivityBody(generation), 3000, generation);
}

