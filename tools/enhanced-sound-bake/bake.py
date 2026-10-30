import argparse
import collections
import glob
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys

LIBS = {
    "footsteps": "FortniteGame/Content/Sounds/Fort_Player/Footsteps/CombatBoots/SoundLib_Footsteps_BootsCombat",
    "clothing": "FortniteGame/Content/Sounds/Fort_Player/Clothing/Generic/Clothing_Generic_Lib",
    "hand_glove": "FortniteGame/Content/Sounds/Fort_Player/Hands/GlovedHand/Hand_Gloved_Lib",
    "hand_bare": "FortniteGame/Content/Sounds/Fort_Player/Hands/BareHand/Hand_Bare_Lib",
}
COLLECTION = "FortniteGame/Content/Sounds/SoundLibrary/DefaultSoundLibraryCollection"
FOOTSTEP_COMPONENT = "FortniteGame/Content/Sounds/Fort_Player/Blueprints/B_FortFootstepComponent"
GAMEPLAY_CUES = {
    "GameplayCue.Athena.Player.Sliding.OneShot.Notify": "FortniteGame/Content/Athena/GameplayCueNotifies/GCN_Sliding_Oneshot",
    "GameplayCue.Athena.Player.Cucumber.Active": "FortniteGame/Content/Athena/GameplayCueNotifies/GCN_Athena_Player_Clamber",
    "GameplayCue.Athena.Player.Sliding.Player": "FortniteGame/Content/Athena/GameplayCueNotifies/GCNL_Athena_Player_Sliding_Player",
}
ENGINE_INI = "FortniteGame/Config/DefaultEngine.ini"
LOW = 0x00000040 if os.name == "nt" else 0


class Uex:
    def __init__(self, a):
        self.a = a

    def run(self, *args):
        cmd = [self.a.dotnet, self.a.uex, *args, "--ue", "5.4", "--usmap", self.a.usmap, "--aes-env", "FN_PAK_AES_KEY"]
        env = dict(os.environ, MSYS_NO_PATHCONV="1")
        r = subprocess.run(cmd, capture_output=True, text=True, env=env, creationflags=LOW)
        return r

    def export(self, game_paths, out):
        if not game_paths:
            return
        os.makedirs(out, exist_ok=True)
        chunk, size = [], 0
        for p in game_paths:
            if size + len(p) > 24000 or len(chunk) >= 12:
                self._export(chunk, out)
                chunk, size = [], 0
            chunk.append(p)
            size += len(p) + 1
        if chunk:
            self._export(chunk, out)

    def _export(self, terms, out):
        r = self.run("export", self.a.game, "--filter", "|".join(terms), "--max", "0", "--out", out)
        if r.returncode != 0:
            print("uex export failed for %d terms: %s" % (len(terms), (r.stderr or r.stdout)[-800:]), file=sys.stderr)

    def raw(self, path, out):
        os.makedirs(out, exist_ok=True)
        self.run("raw", self.a.game, path, "--out", out)


def game_path(object_path):
    p = object_path.split(".")[0]
    if p.startswith("/Game/"):
        return "FortniteGame/Content/" + p[len("/Game/"):]
    if p.startswith("/Engine/"):
        return "Engine/Content/" + p[len("/Engine/"):]
    return p.lstrip("/")


def asset_key(object_path):
    return object_path.split(".")[0]


def load_exports(path):
    j = json.load(open(path, encoding="utf-8"))
    if isinstance(j, dict):
        return j.get("exports", [])
    return j


def json_of(out, gp):
    f = os.path.join(out, *gp.split("/")) + ".json"
    return f if os.path.isfile(f) else None


def ref_name(r):
    if not isinstance(r, dict) or not r.get("ObjectName"):
        return None
    n = r["ObjectName"].split("'")[1]
    return n.split(":")[-1].split(".")[-1]


def notify_table(seq_dir, montage_dir):
    files = sorted(glob.glob(os.path.join(seq_dir, "*.json"))) + sorted(glob.glob(os.path.join(montage_dir, "*_M.json")))
    table = {}
    gcs = set()
    for f in files:
        ex = load_exports(f)
        host = [e for e in ex if e.get("Type") in ("AnimSequence", "AnimMontage")]
        if not host:
            continue
        h = host[0]
        name = h.get("Name")
        rows = []
        for n in h.get("Properties", {}).get("Notifies", []) or []:
            ref = n.get("Notify") or n.get("NotifyStateClass")
            nm = ref_name(ref)
            if not nm:
                continue
            ne = next((e for e in ex if e.get("Name") == nm), {})
            cls = ne.get("Type", "")
            props = ne.get("Properties", {})
            row = {"t": n.get("LinkValue", 0.0), "d": n.get("Duration", 0.0), "chance": n.get("NotifyTriggerChance", 1.0), "thr": n.get("TriggerWeightThreshold", 1e-5)}
            if cls in ("AnimNotify_SoundLibrary", "AnimNotifyState_SoundLibrary"):
                cn = ref_name(props.get("SoundContext"))
                cands = [e for e in ex if e.get("Name") == cn and (e.get("Outer") or {}).get("ObjectName", "").endswith(nm + "'")] or [e for e in ex if e.get("Name") == cn]
                cp = cands[0].get("Properties", {}) if cands else {}
                s = cp.get("PlayerFoleySettings", {})
                row.update(k="state" if "State" in cls else "lib", ev=s.get("EventName", {}).get("TagName"), vol=s.get("VolumeMultiplier", 1.0), pitch=s.get("PitchMultiplier", 1.0))
                if "Surface" in s:
                    row["surf"] = int(str(s["Surface"]).rsplit(":", 1)[-1])
                if "bUseSurfaceType" in s:
                    row["use_surf"] = bool(s["bUseSurfaceType"])
                if "Stance" in s:
                    row["stance"] = int(str(s["Stance"]).rsplit(":", 1)[-1])
            elif cls.startswith("AnimNotify_FootStep"):
                row.update(k="step", ev="SoundLibrary.Foley.Step", foot="r" if "Right" in cls else "l")
            elif cls == "FortAnimNotify_GameplayCue":
                tag = props.get("Cue", {}).get("GameplayCueTag", {}).get("TagName")
                row.update(k="gc", gc=tag)
                gcs.add(tag)
            elif cls == "FortAnimNotify_PlaySoundLocalOnly":
                row.update(k="local", cue=asset_key(props["Sound"]["ObjectPath"]), follow=bool(props.get("bFollow", False)), vol=props.get("VolumeMultiplier", 1.0), pitch=props.get("PitchMultiplier", 1.0))
            else:
                continue
            rows.append(row)
        if rows:
            rows.sort(key=lambda r: r["t"])
            table[name] = rows
    return table, gcs


INTERP = {"RCIM_Linear": 0, "RCIM_Constant": 1, "RCIM_Cubic": 2, "RCIM_None": 3}


def curve_keys(c):
    keys = ((c or {}).get("EditorCurveData") or {}).get("Keys") or []
    return [[float(k.get("Time", 0.0)), float(k.get("Value", 0.0)), INTERP.get(str(k.get("InterpMode", "RCIM_Linear")).rsplit("::", 1)[-1], 0), float(k.get("ArriveTangent", 0.0)), float(k.get("LeaveTangent", 0.0))] for k in keys]


NODE_DEFAULTS = {"PitchMin": 0.95, "PitchMax": 1.05, "VolumeMin": 0.95, "VolumeMax": 1.05}


def convert_cue(ex, waves, atts, classes):
    by = {e.get("Name"): e for e in ex}
    head = next((e for e in ex if "FirstNode" in e.get("Properties", {})), None)
    if head is None:
        return None
    seen = {}

    def node(nm, depth=0):
        e = by.get(nm)
        if e is None or depth > 64:
            return None
        t = e["Type"]
        p = e.get("Properties", {})
        kids = [node(ref_name(c), depth + 1) for c in (p.get("ChildNodes") or [])]
        kids = [k if k is not None else {"k": "null"} for k in kids]
        if t == "SoundNodeWavePlayer":
            w = p.get("SoundWaveAssetPtr", {}).get("AssetPathName")
            if not w:
                return {"k": "wave", "wave": None, "loop": bool(p.get("bLooping", False))}
            key = asset_key(w)
            waves.add(key)
            return {"k": "wave", "wave": key, "loop": bool(p.get("bLooping", False))}
        if t == "SoundNodeRandom":
            ws = p.get("Weights") or []
            ws = [float(x) for x in ws] + [1.0] * max(0, len(kids) - len(ws))
            return {"k": "random", "w": ws[: len(kids)], "norep": bool(p.get("bRandomizeWithoutReplacement", False)), "c": kids}
        if t == "SoundNodeMixer":
            iv = [float(x) for x in (p.get("InputVolume") or [])]
            iv += [1.0] * max(0, len(kids) - len(iv))
            return {"k": "mixer", "v": iv[: len(kids)], "c": kids}
        if t == "SoundNodeConcatenator":
            iv = [float(x) for x in (p.get("InputVolume") or [])]
            iv += [1.0] * max(0, len(kids) - len(iv))
            return {"k": "concat", "v": iv[: len(kids)], "c": kids}
        if t == "SoundNodeModulator":
            g = lambda n: float(p.get(n, NODE_DEFAULTS[n]))
            return {"k": "mod", "p": [g("PitchMin"), g("PitchMax")], "v": [g("VolumeMin"), g("VolumeMax")], "c": kids}
        if t == "SoundNodeAttenuation":
            a = None
            if p.get("bOverrideAttenuation"):
                a = "inline:" + hashlib.sha1(json.dumps(p.get("AttenuationOverrides", {}), sort_keys=True).encode()).hexdigest()[:12]
                atts[a] = p.get("AttenuationOverrides", {})
            elif p.get("AttenuationSettings"):
                a = asset_key(p["AttenuationSettings"]["ObjectPath"])
                atts.setdefault(a, None)
            return {"k": "att", "att": a, "c": kids}
        if t == "SoundNodeSwitch":
            return {"k": "switch", "param": p.get("IntParameterName", "None"), "c": kids}
        if t == "SoundNodeBranch":
            return {"k": "branch", "param": p.get("BoolParameterName", "None"), "c": kids}
        if t in ("SoundNodeParamCrossFade", "SoundNodeDistanceCrossFade"):
            fr = [{"is": float(d.get("FadeInDistanceStart", 0.0)), "ie": float(d.get("FadeInDistanceEnd", 0.0)), "os": float(d.get("FadeOutDistanceStart", 0.0)), "oe": float(d.get("FadeOutDistanceEnd", 0.0)), "v": float(d.get("Volume", 1.0))} for d in (p.get("CrossFadeInput") or [])]
            if t == "SoundNodeParamCrossFade":
                return {"k": "pxfade", "param": p.get("ParamName", "None"), "in": fr, "c": kids}
            return {"k": "dxfade", "in": fr, "c": kids}
        if t == "SoundNodeQualityLevel":
            return {"k": "quality", "c": kids}
        if t == "SoundNodeSoundClass":
            c = p.get("SoundClassOverride")
            ck = asset_key(c["ObjectPath"]) if c else None
            if ck:
                classes.add(ck)
            return {"k": "class", "cls": ck, "c": kids}
        if t == "SoundNodeLooping":
            return {"k": "loop", "n": int(p.get("LoopCount", 1)), "inf": bool(p.get("bLoopIndefinitely", True)), "c": kids}
        if t == "SoundNodeDelay":
            return {"k": "delay", "min": float(p.get("DelayMin", 0.0)), "max": float(p.get("DelayMax", 0.0)), "c": kids}
        if t == "SoundNodeModulatorContinuous":
            return {"k": "modc", "c": kids}
        if t == "SoundNodeEnveloper":
            return {"k": "env", "vol": curve_keys(p.get("VolumeCurve")), "pitch": curve_keys(p.get("PitchCurve")), "loop": bool(p.get("bLoop", False)), "ls": float(p.get("LoopStart", 0.0)), "le": float(p.get("LoopEnd", 0.0)), "c": kids}
        return {"k": "unknown", "cls": t, "c": kids}

    hp = head.get("Properties", {})
    root = node(ref_name(hp.get("FirstNode")))
    cue_cls = hp.get("SoundClassObject")
    ck = asset_key(cue_cls["ObjectPath"]) if cue_cls else None
    if ck:
        classes.add(ck)
    att = None
    if hp.get("bOverrideAttenuation"):
        att = "inline:" + hashlib.sha1(json.dumps(hp.get("AttenuationOverrides", {}), sort_keys=True).encode()).hexdigest()[:12]
        atts[att] = hp.get("AttenuationOverrides", {})
    elif hp.get("AttenuationSettings"):
        att = asset_key(hp["AttenuationSettings"]["ObjectPath"])
        atts.setdefault(att, None)
    return {"name": head.get("Name"), "type": head.get("Type"), "vol": float(hp.get("VolumeMultiplier", 1.0)), "pitch": float(hp.get("PitchMultiplier", 1.0)), "priority": float(hp.get("Priority", 1.0)), "duration": float(hp.get("Duration", 0.0)), "max_distance": float(hp.get("MaxDistance", 0.0)), "class": ck, "att": att, "root": root}


def att_settings(p):
    keep = ["bAttenuate", "bSpatialize", "AttenuationShape", "AttenuationShapeExtents", "FalloffDistance", "DistanceAlgorithm", "dBAttenuationAtMax", "CustomAttenuationCurve", "OmniRadius", "StereoSpread", "bAttenuateWithLPF", "LPFRadiusMin", "LPFRadiusMax", "LPFFrequencyAtMin", "LPFFrequencyAtMax", "bEnableListenerFocus", "FocusAzimuth", "NonFocusAzimuth", "FocusVolumeAttenuation", "NonFocusVolumeAttenuation"]
    out = {}
    for k in keep:
        if k in p:
            v = p[k]
            if isinstance(v, str):
                v = v.rsplit("::", 1)[-1]
            if k == "AttenuationShapeExtents" and isinstance(v, dict):
                v = [v.get("X", 0.0), v.get("Y", 0.0), v.get("Z", 0.0)]
            if k == "CustomAttenuationCurve" and isinstance(v, dict):
                keys = (v.get("EditorCurveData") or {}).get("Keys") or []
                v = [[float(x.get("Time", 0.0)), float(x.get("Value", 0.0))] for x in keys]
            out[k] = v
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--game", default=r"D:\Fortnite\30.0 Forntie\30.00-CL-33962396")
    ap.add_argument("--usmap", default=r"D:\Fortnite\FortniteRef30\usmap\fn30.usmap")
    ap.add_argument("--dotnet", default=os.path.expanduser(r"~\.dotnet\dotnet.exe"))
    ap.add_argument("--uex", default=os.path.expanduser(r"~\.claude\skills\extracting-unreal-assets\tool\bin\uex\uex.dll"))
    ap.add_argument("--vgmstream", default=r"D:\Fortnite\tools\vgmstream\bin\vgmstream-cli.exe")
    ap.add_argument("--ffmpeg", default=shutil.which("ffmpeg") or "ffmpeg")
    ap.add_argument("--seq", default=r"D:\fn-verify\enhsound\seq")
    ap.add_argument("--montages", default=r"D:\fn-verify\enhanim\raw")
    ap.add_argument("--work", default=r"D:\fn-verify\enhsound\work")
    ap.add_argument("--out", default=r"D:\Fortnite\FortniteRust\FortniteRuntime\assets\sound-enhanced")
    ap.add_argument("--quality", default="6")
    a = ap.parse_args()
    if not os.environ.get("FN_PAK_AES_KEY"):
        sys.exit("FN_PAK_AES_KEY is not set")
    u = Uex(a)
    w = a.work
    os.makedirs(w, exist_ok=True)
    jdir = os.path.join(w, "json")
    u.export([COLLECTION, FOOTSTEP_COMPONENT, *LIBS.values(), *GAMEPLAY_CUES.values()], jdir)
    u.raw(ENGINE_INI, os.path.join(w, "cfg"))
    ini = open(os.path.join(w, "cfg", *ENGINE_INI.split("/")), encoding="utf-8", errors="replace").read()
    phys = {int(m.group(1)): m.group(2) for m in re.finditer(r'\+PhysicalSurfaces=\(Type=SurfaceType(\d+),Name="([^"]+)"\)', ini)}
    notifies, gc_tags = notify_table(a.seq, a.montages)
    events = sorted({r["ev"] for rows in notifies.values() for r in rows if r.get("ev")})
    libs = {}
    cue_paths = set()
    for lk, gp in LIBS.items():
        f = json_of(jdir, gp)
        p = load_exports(f)[0]["Properties"]
        m = {}
        for s in p.get("FullLibrary") or p.get("Sounds") or []:
            tag = s["Key"]["TagName"]
            if any(tag == e or tag.startswith(e + ".") for e in events):
                ck = asset_key(s["Value"]["AssetPathName"])
                m[tag] = ck
                cue_paths.add(ck)
        libs[lk] = {"asset": gp, "tags": m, "channel": (p.get("Channel") or {}).get("TagName"), "priority": p.get("Priority", 0)}
    gcs = {}
    for tag, gp in GAMEPLAY_CUES.items():
        f = json_of(jdir, gp)
        if not f:
            continue
        ex = load_exports(f)
        cdo = next((e for e in ex if e.get("Name", "").startswith("Default__")), {})
        p = cdo.get("Properties", {})
        entry = {"asset": gp, "burst": [], "loop": None, "start": None, "stop": None}
        for bs in (p.get("BurstEffectData") or {}).get("BurstSounds") or []:
            if bs.get("SoundCue"):
                ck = asset_key(bs["SoundCue"]["ObjectPath"])
                cue_paths.add(ck)
                entry["burst"].append({"cue": ck, "delay": bs.get("DelayBeforePlayInSeconds", 0.0), "chance": (bs.get("Condition") or {}).get("ChanceToPlay", 1.0)})
        for k, prop in (("start", "StartSlideSound"), ("stop", "StopSlideSound")):
            if p.get(prop):
                entry[k] = asset_key(p[prop]["ObjectPath"])
                cue_paths.add(entry[k])
        lac = next((e for e in ex if e.get("Type") == "FortLayeredAudioComponent"), None)
        if lac and lac.get("Properties", {}).get("Sound"):
            lp = lac["Properties"]
            entry["loop"] = {"cue": asset_key(lp["Sound"]["ObjectPath"]), "params": [{"name": fp["Name"], "attack": fp.get("AttackSpeed", 0.0), "release": fp.get("ReleaseSpeed", 0.0), "range": [fp.get("ParamRange", {}).get("X", 0.0), fp.get("ParamRange", {}).get("Y", 1.0)]} for fp in lp.get("FloatParameters", []) if fp.get("bEnabled", True)]}
            cue_paths.add(entry["loop"]["cue"])
        gcs[tag] = entry
    for rows in notifies.values():
        for r in rows:
            if r.get("cue"):
                cue_paths.add(r["cue"])
    cdir = os.path.join(w, "cues")
    u.export([game_path(c) for c in sorted(cue_paths)], cdir)
    waves, atts, classes = set(), {}, set()
    cues = {}
    for c in sorted(cue_paths):
        f = json_of(cdir, game_path(c))
        if not f:
            print("missing cue", c)
            continue
        cv = convert_cue(load_exports(f), waves, atts, classes)
        if cv:
            cues[c] = cv
    adir = os.path.join(w, "att")
    u.export([game_path(x) for x in atts if not x.startswith("inline:")], adir)
    att_out = {}
    for k, v in atts.items():
        if v is None:
            f = json_of(adir, game_path(k))
            if not f:
                print("missing attenuation", k)
                continue
            v = load_exports(f)[0].get("Properties", {}).get("Attenuation", {})
        att_out[k] = att_settings(v)
    sdir = os.path.join(w, "cls")
    todo, cls_out = set(classes), {}
    while todo:
        u.export([game_path(x) for x in todo], sdir)
        nxt = set()
        for k in todo:
            f = json_of(sdir, game_path(k))
            if not f:
                print("missing class", k)
                continue
            p = load_exports(f)[0].get("Properties", {})
            pr = p.get("Properties", {})
            parent = asset_key(p["ParentClass"]["ObjectPath"]) if p.get("ParentClass") else None
            cls_out[k] = {"volume": float(pr.get("Volume", 1.0)), "pitch": float(pr.get("Pitch", 1.0)), "parent": parent, "ui": bool(pr.get("bIsUISound", False)), "always": bool(pr.get("bAlwaysPlay", False))}
            if parent and parent not in cls_out:
                nxt.add(parent)
        todo = nxt - set(cls_out)
    wdir = os.path.join(w, "waves")
    dirs = sorted({game_path(x).rsplit("/", 1)[0] + "/" for x in waves})
    for d in dirs:
        u.export([d], wdir)
    wav_info = {}
    for x in sorted(waves):
        gp = game_path(x)
        f = json_of(wdir, gp)
        if f:
            p = load_exports(f)[0].get("Properties", {})
            wav_info[x] = {"volume": float(p.get("Volume", 1.0)), "pitch": float(p.get("Pitch", 1.0)), "looping": bool(p.get("bLooping", False))}
    names = collections.Counter(x.rsplit("/", 1)[-1].lower() for x in waves)
    odir = os.path.join(w, "ogg")
    os.makedirs(odir, exist_ok=True)
    os.makedirs(a.out, exist_ok=True)
    pack = open(os.path.join(a.out, "waves.pack"), "wb")
    tmp = os.path.join(w, "tmp")
    os.makedirs(tmp, exist_ok=True)
    wave_out = {}
    total, secs = 0, 0.0
    for x in sorted(waves):
        gp = game_path(x)
        src = os.path.join(wdir, *gp.split("/")) + ".binka"
        if not os.path.isfile(src):
            alt = glob.glob(os.path.join(wdir, *gp.split("/")) + ".*")
            alt = [p for p in alt if not p.endswith(".json")]
            if not alt:
                print("missing wave", x)
                continue
            src = alt[0]
        base = x.rsplit("/", 1)[-1]
        if names[base.lower()] > 1:
            base += "_" + hashlib.sha1(x.encode()).hexdigest()[:6]
        dst = os.path.join(odir, base + ".ogg")
        wav = os.path.join(tmp, base + ".wav")
        meta = subprocess.run([a.vgmstream, "-m", src], capture_output=True, text=True, creationflags=LOW).stdout
        rate = int(re.search(r"sample rate: (\d+)", meta).group(1))
        ch = int(re.search(r"channels: (\d+)", meta).group(1))
        frames = int(re.search(r"stream total samples: (\d+)", meta).group(1))
        if not os.path.isfile(dst):
            subprocess.run([a.vgmstream, "-o", wav, src], capture_output=True, creationflags=LOW, check=True)
            subprocess.run([a.ffmpeg, "-hide_banner", "-loglevel", "error", "-y", "-i", wav, "-c:a", "libvorbis", "-q:a", a.quality, dst], creationflags=LOW, check=True)
            os.remove(wav)
        total += os.path.getsize(dst)
        secs += frames / rate
        info = wav_info.get(x, {"volume": 1.0, "pitch": 1.0, "looping": False})
        data = open(dst, "rb").read()
        wave_out[x] = {"offset": pack.tell(), "size": len(data), "rate": rate, "channels": ch, "frames": frames, **info}
        pack.write(data)
    pack.close()
    old = os.path.join(a.out, "waves")
    if os.path.isdir(old):
        shutil.rmtree(old)
    index = {
        "source": "Fortnite 30.00 CL-33962396",
        "physical_surfaces": {str(k): v for k, v in sorted(phys.items())},
        "libraries": libs,
        "gameplay_cues": gcs,
        "notifies": notifies,
        "cues": cues,
        "attenuations": att_out,
        "classes": cls_out,
        "waves": wave_out,
    }
    json.dump(index, open(os.path.join(a.out, "index.json"), "w", encoding="utf-8"), separators=(",", ":"), sort_keys=True)
    print(f"{len(cues)} cues, {len(wave_out)} waves ({total / 1048576:.1f} MB ogg, {secs:.1f} s audio), {len(att_out)} attenuations, {len(cls_out)} classes, {len(notifies)} clips with notifies, events {events}")


if __name__ == "__main__":
    main()
