"""scripts/patch_downloads.py writing qBittorrent's web login (init, APPS_AUTH apps or
tunnel): qBittorrent 5 reads it from [WebUI] and ignores 4.x's [Preferences] WebUI\\..."""
import os
import subprocess
import sys
from pathlib import Path

import pytest

SCRIPT = Path(__file__).resolve().parent.parent / "scripts" / "patch_downloads.py"

# LinuxServer's /defaults/qBittorrent.conf, which a new install starts from
LSIO_TEMPLATE = """[AutoRun]
enabled=false
program=

[LegalNotice]
Accepted=true

[Preferences]
Connection\\UPnP=false
Connection\\PortRangeMin=6881
Downloads\\SavePath=/downloads/
Downloads\\TempPath=/downloads/incomplete/
WebUI\\Address=*
WebUI\\ServerDomains=*
"""

# as qBittorrent 5.3 saves it
V5 = """[Meta]
MigrationVersion=12

[Preferences]
Connection\\ResolvePeerCountries=true

[WebUI]
Address=*
ServerDomains=*
"""


# saved by qBittorrent 5.3 (it has converted the file: MigrationVersion), still with
# 4.x keys left over and no [WebUI] yet. 5.3 doesn't convert again: it ignores them.
MIGRATED = """[LegalNotice]
Accepted=true

[Meta]
MigrationVersion=12

[Preferences]
WebUI\\Address=*
WebUI\\ServerDomains=*
"""


def patch(tmp: Path, conf: str, apps_auth="tunnel", password="s3cret-pass") -> str:
    f = tmp / "qbittorrent" / "qBittorrent" / "qBittorrent.conf"
    f.parent.mkdir(parents=True)
    f.write_text(conf)
    env = dict(os.environ, DL_APPS="qbittorrent", APPS_AUTH=apps_auth,
               QBITTORRENT_USER="bob", QBITTORRENT_PASSWORD=password)
    subprocess.run([sys.executable, str(SCRIPT), str(tmp)], env=env, check=True, capture_output=True)
    return f.read_text()


def section(text: str, name: str) -> dict[str, str]:
    out, cur = {}, None
    for line in text.splitlines():
        s = line.strip()
        if s.startswith("[") and s.endswith("]"):
            cur = s[1:-1]
        elif cur == name and "=" in line:
            k, _, v = line.partition("=")
            out[k] = v
    return out


def login_in(sec: dict[str, str], pre: str):
    assert sec.get(pre + "Username") == "bob"
    assert sec.get(pre + "Password_PBKDF2", "").startswith('"@ByteArray(')
    assert sec.get(pre + "AuthSubnetWhitelistEnabled") == "true"
    assert "192.168.0.0/16" in sec.get(pre + "AuthSubnetWhitelist", "")


def test_a_new_install_still_on_the_template_gets_the_login_where_qbittorrent_5_reads_it(tmp_path):
    out = patch(tmp_path, LSIO_TEMPLATE)
    login_in(section(out, "WebUI"), "")
    # and where 4.x reads it, since the template doesn't say which version it is
    login_in(section(out, "Preferences"), "WebUI\\")


def test_a_converted_file_with_4x_leftovers_gets_the_login_in_webui(tmp_path):
    # what used to go wrong: the leftovers made it look like 4.x, so only they got the login
    login_in(section(patch(tmp_path, MIGRATED), "WebUI"), "")


def test_a_qbittorrent_5_file_gets_only_the_webui_section(tmp_path):
    out = patch(tmp_path, V5)
    login_in(section(out, "WebUI"), "")
    assert not any(k.startswith("WebUI\\") for k in section(out, "Preferences"))


def test_rerunning_changes_nothing(tmp_path):
    out = patch(tmp_path, LSIO_TEMPLATE)
    f = tmp_path / "again" / "qbittorrent" / "qBittorrent" / "qBittorrent.conf"
    f.parent.mkdir(parents=True)
    f.write_text(out)
    env = dict(os.environ, DL_APPS="qbittorrent", APPS_AUTH="tunnel",
               QBITTORRENT_USER="bob", QBITTORRENT_PASSWORD="s3cret-pass")
    r = subprocess.run([sys.executable, str(SCRIPT), str(tmp_path / "again")], env=env, check=True,
                       capture_output=True, text=True)
    assert f.read_text() == out
    assert "nothing to change" in r.stdout


@pytest.mark.parametrize("conf", [LSIO_TEMPLATE, V5], ids=["template", "v5"])
def test_without_a_password_the_login_is_left_alone(tmp_path, conf):
    out = patch(tmp_path, conf, apps_auth="apps", password="")
    assert "Password_PBKDF2" not in out
    assert section(out, "WebUI").get("AuthSubnetWhitelistEnabled") == "false"
