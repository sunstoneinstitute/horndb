# OWL 2 RL hand-encoded fixtures

These fixtures mirror the structure of W3C OWL 2 RL test cases without depending on
the W3C manifest format (which the SPEC-01 conformance harness owns). Each test
asserts a base graph, runs `materialize`, and checks for specific expected and
forbidden triples.

When the SPEC-01 harness wiring lands, the canonical W3C cases will exercise this
engine through the harness — these tests will remain as a fast inner-loop sanity
check that does not require the harness binary.
