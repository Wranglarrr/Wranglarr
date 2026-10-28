"""Community configs against a mock Radarr: check, apply (a new tag, a changed custom
format), and an undo that partly fails keeps only the failed change, so
pressing Undo again finishes it."""
import json

import pytest

from harness import ARR_KEY, MockArr, Server, base_toml, suite_toml

CF_OLD = {"id": 2, "name": "cf", "includeCustomFormatWhenRenaming": False, "specifications": [
    {"name": "title", "implementation": "ReleaseTitleSpecification", "negate": False, "required": False,
     "fields": [{"name": "value", "value": "old"}]}]}
CONFIG = {"wranglarr": "wranglarr-config/1", "name": "test config", "apps": {"radarr": {
    "tags": ["x"],
    "customFormats": [{"name": "cf", "specifications": [
        {"name": "title", "implementation": "ReleaseTitleSpecification", "fields": {"value": "new"}}]}]}}}


@pytest.fixture(scope="module")
def env(tmp_path_factory):
    tmp = tmp_path_factory.mktemp("community")
    arr = MockArr(tmp, "mock-radarr", "--empty")
    arr.seed(customformat=[CF_OLD], tag=[], qualityprofile=[],
             static={"/api/v3/customformat/schema": [
                 {"implementation": "ReleaseTitleSpecification", "name": "Release Title", "fields": [{"name": "value", "value": ""}]}]})
    srv = Server(tmp, base_toml(tmp) + suite_toml("media", [dict(name="radarr", kind="radarr", url=arr.url, api_key=ARR_KEY)])).start()
    c = srv.client()
    yield type("Env", (), dict(arr=arr, c=c, ctx={}))
    c.close()
    srv.stop()
    arr.stop()


def history(c, hid):
    return next(h for h in c.get("/api/configs").json()["history"] if h["id"] == hid)


def test_check_and_apply(env):
    c, arr = env.c, env.arr
    r = c.post("/api/configs/check", json={"text": json.dumps(CONFIG)})
    assert r.status_code == 200, r.text
    rows = {(row["part"], row["name"]): row["action"] for row in r.json()["plans"][0]["rows"]}
    assert rows == {("tags", "x"): "create", ("customFormats", "cf"): "update"}, rows
    assert c.post("/api/configs/check", json={"text": "not json"}).status_code == 400
    r = c.post("/api/configs/apply", json={"text": json.dumps(CONFIG)})
    assert r.status_code == 200, r.text
    res = r.json()["results"][0]
    assert res["ok"] and res["changed"] == 2 and res["history"], res
    store = arr.state()["store"]
    assert [t["label"] for t in store["tag"]] == ["x"]
    assert store["customformat"][0]["specifications"][0]["fields"] == [{"name": "value", "value": "new"}]
    env.ctx["hid"] = res["history"]


def test_partial_undo_keeps_what_failed(env):
    c, arr, hid = env.c, env.arr, env.ctx["hid"]
    arr.fault(method="PUT", prefix="/api/v3/customformat", status=500, body={"message": "server error"})
    try:
        r = c.post(f"/api/configs/history/{hid}/undo")
        assert r.status_code == 200, r.text
    finally:
        arr.clear_faults()
    h = history(c, hid)
    assert not h.get("undone"), ("a partial undo isn't marked undone", h)
    assert [i["name"] for i in h["items"]] == ["cf"], ("only the failed item is left", h["items"])
    assert arr.state()["store"]["tag"] == [], "the tag it made is gone"
    r = c.post(f"/api/configs/history/{hid}/undo")              # again: finishes it
    assert r.status_code == 200, r.text
    assert history(c, hid).get("undone"), "the retry finishes the undo"
    assert arr.state()["store"]["customformat"][0] == CF_OLD, "the custom format is back as it was"
    r = c.post(f"/api/configs/history/{hid}/undo")
    assert r.status_code == 400 and "already undone" in r.json()["error"], r.text
