# system-paths — DESIGN

## Intent

No absolute path makes it into compiled code, except here. `spira-lint`'s `compile-time-path`
rule refuses every string literal that starts with an absolute path, and exempts this crate by
name. The operator's ruling of 2026-11-16: "no absolute paths make it into compiled code. i
don't even think there should be an exception for fixtures. relative paths only."

## What may live here

A path the code must name absolutely at run time: a host path (`/proc/pressure/io`, a system
unit directory, a binary directory) or, in `guest`, a path absolute inside a container or VM
this code builds. Name each constant by what it is, not by who reads it. Anything else is built
from a root at run time.

## What may not

A fixture or a made-up root (`/r/x`, `/home/x`). A fixture is relative, or joined to a
`testkit::TempDir`. A D-Bus object path, URL path or route is not a filesystem path and is not
named here either; mark it `// not a path: <reason>` where it stands.
