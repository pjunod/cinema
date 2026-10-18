// amd64 package smoke: build the audited Bookworm media package and the
// runtime that consumes it in ONE BuildKit solve. The runtime receives the
// package as the `linux-dolby-package` named context straight from the
// `linux-dolby-package` target, so nothing is exported to the client. In two
// full runs (5228, 5325) the previous `type=local` export of the package
// stalled in "exporting to client directory / copying files" until the job
// deadline, while the host sat idle.

variable "BUILD_REF" { default = "" }
variable "SOURCE_URL" { default = "" }

target "linux-dolby-package" {
  context    = "."
  dockerfile = "Dockerfile"
  target     = "linux-dolby-package-context"
  platforms  = ["linux/amd64"]
}

target "runtime-amd64" {
  context    = "."
  dockerfile = "Dockerfile.release"
  contexts = {
    linux-dolby-package = "target:linux-dolby-package"
  }
  platforms = ["linux/amd64"]
  tags      = ["plurx/plurxd:ci-amd64"]
  labels = {
    "org.opencontainers.image.source"   = SOURCE_URL
    "org.opencontainers.image.revision" = BUILD_REF
    "tv.plurx.build-ref"                = BUILD_REF
  }
  output = ["type=docker"]
}
