#!/usr/bin/env bash
# Deliberate hosted-proof mutation. This copy exits without reading policy.
# It must not be merged.
printf '%s\n' 'candidate checker intentionally returns success'
exit 0
