#!/usr/bin/env python3
"""Point each arr's config.xml at Postgres.

Usage: patch_config.py <dir containing <app>/config.xml for each app in ARR_APPS>

Reads POSTGRES_HOST / POSTGRES_PORT / POSTGRES_USER / POSTGRES_PASSWORD from the
environment and inserts or updates these elements under <Config>:

    <PostgresHost> <PostgresPort> <PostgresUser> <PostgresPassword>
    <PostgresMainDb> <PostgresLogDb>

The arrs read these keys but never write them back (they are loaded with
persist=false), so a freshly generated config.xml will not contain them; this
script adds them.  Idempotent: re-running only changes values that differ.
It refuses to touch a config that uses the ConnectionString variant, since
mixing the two styles is a startup error.
"""
import os
import sys
import xml.etree.ElementTree as ET

KEYS = ("PostgresHost", "PostgresPort", "PostgresUser", "PostgresPassword",
        "PostgresMainDb", "PostgresLogDb")
CONNSTR_KEYS = ("PostgresMainDbConnectionString", "PostgresLogDbConnectionString")


def env(name, default=None):
    value = os.environ.get(name, default)
    if value is None or value == "":
        sys.exit(f"patch_config: {name} must be set")
    return value


def patch(path, app, settings):
    if not os.path.exists(path):
        print(f"[{app}] {path} missing; skipping (start the app once first)")
        return False

    tree = ET.parse(path)
    root = tree.getroot()
    if root.tag != "Config":
        sys.exit(f"[{app}] unexpected root element <{root.tag}> in {path}")

    for key in CONNSTR_KEYS:
        node = root.find(key)
        if node is not None and (node.text or "").strip():
            print(f"[{app}] uses <{key}>; leaving config.xml untouched")
            return False

    changed = []
    for key, value in settings.items():
        node = root.find(key)
        if node is None:
            node = ET.SubElement(root, key)
            node.text = value
            changed.append(f"+{key}")
        elif (node.text or "") != value:
            node.text = value
            changed.append(f"~{key}")

    if not changed:
        print(f"[{app}] already configured for Postgres")
        return True

    ET.indent(root, space="  ")
    tmp = path + ".tmp"
    tree.write(tmp, encoding="utf-8", xml_declaration=False)
    with open(tmp, "a") as fh:
        fh.write("\n")
    os.replace(tmp, path)
    print(f"[{app}] patched: {' '.join(changed)}")
    return True


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    base = sys.argv[1]
    apps = env("ARR_APPS", "radarr sonarr lidarr").split()
    common = {
        "PostgresHost": env("POSTGRES_HOST", "postgres"),
        "PostgresPort": env("POSTGRES_PORT", "5432"),
        "PostgresUser": env("POSTGRES_USER"),
        "PostgresPassword": env("POSTGRES_PASSWORD"),
    }
    ok = True
    for app in apps:
        settings = dict(common, PostgresMainDb=app, PostgresLogDb=f"{app}-log")
        ok &= patch(os.path.join(base, app, "config.xml"), app, settings)
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
