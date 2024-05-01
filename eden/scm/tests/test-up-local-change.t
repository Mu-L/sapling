test updating backwards through a rename is not supported yet. `update` is not a
common use case for copy tracing, and enable copy tracing can impact the performance
for long distance update. Time will tell if we really need it.
  $ hg log -G -T '{node|short} {desc}' -p --git
  @  fdbc53b96b17 bdiff --git a/a b/b
  │  rename from a
  │  rename to b
  │
  o  cb9a9f314b8b adiff --git a/a b/a
     new file mode 100644
     --- /dev/null
     +++ b/a
     @@ -0,0 +1,1 @@
     +a

For update, base=fdbc53b96b17, src=cb9a9f314b8b, dst=fdbc53b96b17

  local [working copy] changed b which other [destination] deleted
  use (c)hanged version, (d)elete, or leave (u)nresolved? u
  [1]
  A b
  diff -r cb9a9f314b8b b
  --- /dev/null
  +++ b/b
  @@ -0,0 +1,1 @@