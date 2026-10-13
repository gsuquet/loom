+++
# Edited by hand: comments, key order and unknown keys must survive.
title   =   'Login timeout on mobile checkout'
id = "k3q7zm2a"
status = "active"   # since the standup
estimate = "2h"     # unknown to this version
kind = "bug"
importance = "p1"
context = "company-a"
created = 2026-10-12T09:14:00Z

[relations]
blocks = ["m4x9pq2r"]
duplicates = ["z9z9z9z9"]
spawned_from = []

[[links]]
role = "repo"
repo = "company-a/checkout"
branch = "fix/login-timeout"

[[links]]
role = "ticket" # a link role written by a newer version
system = "freshservice"

[agent]
model = "opus"
+++

## Symptoms

Checkout times out after 30 s on mobile.

+++ not a delimiter, part of the notes
