# SPDX-License-Identifier: Apache-2.0
"""
cc_blender_open.py -- "Open in Blender" (0.8 RE.4 / RE.6): the same scene the Blender engine renders, left open in Blender's window
for people who want to keep going there.  Shipped beside cc_blender_render.py and started only by Studio's engines.rs:

    blender --python cc_blender_open.py -- <job file>        (no --background, no --factory-startup: the person's own Blender)

The job file is the same cc-engine-job.json (typed fields written by Rust).  It builds the scene (USD, then the manifest over it) and
stops: no render.  On a problem it prints CCERROR <code> and leaves Blender open on whatever was made.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cc_blender_render as R  # noqa: E402


def main(argv):
    path = argv[argv.index('--') + 1]
    folder = os.path.dirname(os.path.abspath(path))
    job = R.load_job(path)
    m = R.load_manifest(folder, job['manifest'])
    bpy, mathutils = R.blender_modules()
    R.build_scene(bpy, mathutils, job, m, folder)


if __name__ == '__main__':
    try:
        main(sys.argv)
    except SystemExit:
        pass   # the window stays open; the line above the exit says why
    except Exception as e:   # noqa: BLE001
        R.say('CCERROR script_failed')
        R.say(type(e).__name__ + ': ' + str(e)[:300])
