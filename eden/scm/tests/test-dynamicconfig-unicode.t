#debugruntest-compatible

#require no-eden


  $ configure modern

  $ export HG_TEST_INTERNALCONFIG="$TESTTMP/test_hgrc"
  $ cat > test_hgrc <<EOF
  > [section]
  > key=✓
  > EOF

  $ hg init client
  $ cd client

Verify it can be manually generated

  $ hg debugrefreshconfig
  $ cat .hg/hgrc.dynamic
  # version=* (glob)
  # reponame=* (glob)
  # canary=None
  # username=
  # Generated by `hg debugrefreshconfig` - DO NOT MODIFY
  [section]
  key=✓
  
  $ hg config section.key
  ✓
