#!/usr/bin/env python3
# census/classmap.py — event_type + cause -> failure-class name. Shared by cluster.py
# (ranked classes) and handwritten.py (actor-excluded classes) so the two pipelines
# cannot name the same event differently.
def class_for(event_type, cause):
    cause = cause or 'unrecorded'
    if event_type == 'requeued':
        return 'sp-requeue-' + cause
    if event_type == 'recurred':
        return 'sp-recur-' + cause
    if event_type == 'reclaimed':
        return 'sp-reclaim-' + cause if cause != 'unrecorded' else 'sp-reclaim'
    if event_type == 'lapsed':
        return 'sp-lapsed-' + cause
    if event_type in ('reopen', 'reopened'):
        return 'sp-reopen-' + cause
    return None
