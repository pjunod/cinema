"use strict";

const assert = require("node:assert/strict");
const test = require("node:test");
const {parseServerMetrics, serverMetricDelta} = require("../../scripts/playback-lab");

const before = `# HELP plurx_hls_requests_total x
# TYPE plurx_hls_requests_total counter
plurx_hls_requests_total{surface="session",resource="segment",role="muxed",status="200"} 4
plurx_hls_requests_total{surface="session",resource="segment",role="audio",status="200"} 0
plurx_vod_segment_answers_total{source="stored"} 10
plurx_http_requests_total{route_group="playback",method="get",status="2xx"} 99
`;

const after = `plurx_hls_requests_total{surface="session",resource="segment",role="muxed",status="200"} 9
plurx_hls_requests_total{surface="session",resource="segment",role="audio",status="200"} 0
plurx_vod_segment_answers_total{source="stored"} 13
plurx_vod_segment_answers_total{source="produced"} 2
plurx_http_requests_total{route_group="playback",method="get",status="2xx"} 140
`;

test("server metric deltas keep only the baseline families that moved", () => {
  const delta = serverMetricDelta(parseServerMetrics(before), parseServerMetrics(after));
  assert.deepEqual(delta, {
    'plurx_hls_requests_total{surface="session",resource="segment",role="muxed",status="200"}': 5,
    'plurx_vod_segment_answers_total{source="stored"}': 3,
    'plurx_vod_segment_answers_total{source="produced"}': 2,
  });
});

test("server metric parsing ignores comments, other families and malformed lines", () => {
  const series = parseServerMetrics("# HELP a\nplurx_hls_requests_total{a=\"b\"} nope\nplurx_other 3\n");
  assert.equal(series.size, 0);
});
