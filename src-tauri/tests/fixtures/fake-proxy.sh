#!/bin/bash
# Fake cloud-sql-proxy for tests.
#   ready (default) : print ready line, then sleep forever
#   bind            : print an address-in-use error, exit 1
#   crash           : print ready line, then exit 1 shortly after
#
# The mode is read from FAKE_PROXY_MODE, which ProxyManager forwards per-child
# from Profile::impersonate_service_account in the tests (see
# tests/proxy_manager.rs) -- the tests do NOT mutate the shared process
# environment, so they are safe to run concurrently.
#
# Readiness is printed to stderr because that is where the real proxy logs it.
set -u
mode="${FAKE_PROXY_MODE:-ready}"
echo "fake-proxy args: $*" >&2
case "$mode" in
  bind)
    echo "2026/08/18 10:00:00 failed to start listener: listen tcp 127.0.0.1:15432: bind: address already in use" >&2
    exit 1
    ;;
  crash)
    echo "2026/08/18 10:00:00 The proxy has started successfully and is ready for new connections!" >&2
    sleep 0.2
    echo "2026/08/18 10:00:01 unexpected shutdown" >&2
    exit 1
    ;;
  *)
    echo "2026/08/18 10:00:00 The proxy has started successfully and is ready for new connections!" >&2
    while true; do sleep 1; done
    ;;
esac
