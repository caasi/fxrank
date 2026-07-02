#!/usr/bin/env bash
GREETING=hello        # top-level → forces a <script> unit

greet() { echo "$GREETING"; }

function deploy {
  outer() { rm -rf /tmp/x; }   # nested definition
  greet
}
