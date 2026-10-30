using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using CUE4Parse.ACL;
using CUE4Parse.Compression;
using CUE4Parse.Encryption.Aes;
using CUE4Parse.FileProvider;
using CUE4Parse.MappingsProvider.Usmap;
using CUE4Parse.UE4.Assets.Exports;
using CUE4Parse.UE4.Assets.Exports.Animation;
using CUE4Parse.UE4.Assets.Exports.Animation.ACL;
using CUE4Parse.UE4.Objects.Core.Math;
using CUE4Parse.UE4.Objects.Core.Misc;
using CUE4Parse.UE4.Versions;
using CUE4Parse_Conversion.Animations;
using Newtonsoft.Json;
using Newtonsoft.Json.Linq;

static class Program
{
    [DllImport(ACLNative.LIB_NAME)]
    static extern unsafe void nReadACLData(IntPtr compressedTracks, FTransform* inRefPoses, FTrackToSkeletonMap* inTrackToSkeletonMap, FTransform* outAtom);

    static int Main(string[] a)
    {
        if (a.Length < 6) { Console.Error.WriteLine("usage: enhanced-anim-dump <gameDir> <usmap> <aesEnvVar> <oodleDll> <outDir> <listFile>"); return 1; }
        string gameDir = a[0], usmap = a[1], aesEnv = a[2], oodle = a[3], outDir = a[4], list = a[5];
        OodleHelper.Initialize(oodle);
        var p = new DefaultFileProvider(gameDir, SearchOption.AllDirectories, new VersionContainer(EGame.GAME_UE5_3), StringComparer.OrdinalIgnoreCase);
        p.ReadScriptData = true;
        p.Initialize();
        p.MappingsContainer = new FileUsmapTypeMappingsProvider(usmap);
        var key = Environment.GetEnvironmentVariable(aesEnv) ?? throw new ArgumentException("aes env var empty");
        p.Mount();
        p.SubmitKeys(new Dictionary<FGuid, FAesKey> { [new FGuid()] = new FAesKey(key.StartsWith("0x") ? key : "0x" + key) });
        p.PostMount();
        Directory.CreateDirectory(outDir);
        int ok = 0, fail = 0;
        foreach (var raw in File.ReadAllLines(list))
        {
            var path = raw.Trim();
            if (path.Length == 0 || path.StartsWith("#")) continue;
            if (path.StartsWith("bp:"))
            {
                path = path.Substring(3);
                try
                {
                    var bexp = p.LoadPackage(path).GetExports().ToList();
                    var bname = Path.GetFileNameWithoutExtension(path);
                    var cls = bexp.OfType<CUE4Parse.UE4.Objects.UObject.UClass>().FirstOrDefault();
                    if (cls != null) File.WriteAllText(Path.Combine(outDir, bname + ".bp.txt"), cls.DecompileBlueprintToPseudo());
                    File.WriteAllText(Path.Combine(outDir, bname + ".bp.json"), JsonConvert.SerializeObject(bexp, Formatting.Indented));
                    Console.WriteLine("ok bp " + bname);
                    ok++;
                }
                catch (Exception e) { Console.Error.WriteLine($"FAIL {path}: {e.GetType().Name}: {e.Message}"); fail++; }
                continue;
            }
            try
            {
                var exports = p.LoadPackage(path).GetExports().ToList();
                var name = Path.GetFileNameWithoutExtension(path);
                JToken outJ;
                var seq = exports.OfType<UAnimSequence>().FirstOrDefault();
                var skel = exports.OfType<USkeleton>().FirstOrDefault();
                if (seq != null) outJ = DumpSequence(seq, path);
                else if (skel != null) outJ = DumpSkeleton(skel, path);
                else outJ = new JObject { ["kind"] = "exports", ["path"] = path, ["exports"] = JToken.Parse(JsonConvert.SerializeObject(exports)) };
                File.WriteAllText(Path.Combine(outDir, name + ".json"), outJ.ToString(Formatting.None));
                Console.WriteLine("ok " + name);
                ok++;
            }
            catch (Exception e) { Console.Error.WriteLine($"FAIL {path}: {e.GetType().Name}: {e.Message}"); fail++; }
        }
        Console.Error.WriteLine($"{ok} ok, {fail} failed");
        return fail > 0 ? 2 : 0;
    }

    static JArray V(FVector v) => new JArray(v.X, v.Y, v.Z);
    static JArray Q(FQuat q) => new JArray(q.X, q.Y, q.Z, q.W);

    static JObject DumpSkeleton(USkeleton s, string path)
    {
        var tree = JToken.Parse(JsonConvert.SerializeObject(s))["Properties"]?["BoneTree"] as JArray;
        int Mode(int i)
        {
            var v = tree != null && i < tree.Count ? tree[i]["TranslationRetargetingMode"]?.ToString() : null;
            if (v == null) return 0;
            var tail = v.Substring(v.LastIndexOf(':') + 1);
            if (int.TryParse(tail, out var n)) return n;
            return tail switch { "BRS_KeepComponentSpaceRotation" => 0, "BRS_KeepLocalSpaceRotation" => 1, "BRS_CopyFromTarget" => 2, "Animation" => 0, "Skeleton" => 1, "AnimationScaled" => 2, "AnimationRelative" => 3, "OrientAndScale" => 4, _ => -1 };
        }
        var bones = new JArray();
        var info = s.ReferenceSkeleton.FinalRefBoneInfo;
        var pose = s.ReferenceSkeleton.FinalRefBonePose;
        for (int i = 0; i < info.Length; i++)
            bones.Add(new JObject { ["name"] = info[i].Name.Text, ["parent"] = info[i].ParentIndex, ["t"] = V(pose[i].Translation), ["r"] = Q(pose[i].Rotation), ["s"] = V(pose[i].Scale3D), ["retarget"] = Mode(i) });
        var sources = new JObject();
        foreach (var kv in s.AnimRetargetSources ?? new Dictionary<CUE4Parse.UE4.Objects.UObject.FName, FReferencePose>())
            sources[kv.Key.Text] = new JArray(kv.Value.ReferencePose.Select(t => new JObject { ["t"] = V(t.Translation), ["r"] = Q(t.Rotation), ["s"] = V(t.Scale3D) }));
        return new JObject { ["kind"] = "skeleton", ["path"] = path, ["bones"] = bones, ["retarget_sources"] = sources };
    }

    static unsafe JObject DumpSequence(UAnimSequence seq, string path)
    {
        var skeleton = seq.Skeleton.Load<USkeleton>();
        var numBones = skeleton.BoneCount;
        FTransform[] basePose = seq.RetargetSource.IsNone
            ? (seq.RetargetSourceAssetReferencePose is { Length: > 0 } ? seq.RetargetSourceAssetReferencePose : null)
            : (skeleton.AnimRetargetSources.TryGetValue(seq.RetargetSource, out var rp) ? rp.ReferencePose : null);
        var refPose = basePose ?? skeleton.ReferenceSkeleton.FinalRefBonePose;
        var bones = new JArray();
        int numKeys;
        string codec;
        if (seq.CompressedDataStructure is FACLCompressedAnimData acl)
        {
            codec = "acl";
            using var tracks = acl.GetCompressedTracks();
            var hdr = tracks.GetTracksHeader();
            int numSamples = (int)hdr.NumSamples;
            if (seq.IsValidAdditive()) tracks.SetDefaultScale(0);
            var trackMap = seq.GetTrackMap();
            int need = trackMap.Length == 0 ? 0 : trackMap.Max(m => m.BoneTreeIndex) + 1;
            if (need > refPose.Length)
            {
                var padded = new FTransform[need];
                for (int i = 0; i < need; i++) padded[i] = i < refPose.Length ? refPose[i] : FTransform.Identity;
                refPose = padded;
            }
            var atoms = new FTransform[trackMap.Length * numSamples];
            fixed (FTransform* refPtr = refPose)
            fixed (FTrackToSkeletonMap* mapPtr = trackMap)
            fixed (FTransform* atomPtr = atoms)
            {
                nReadACLData(tracks.Handle, refPtr, mapPtr, atomPtr);
            }
            bool wraps = hdr.GetIsWrapOptimized() && numSamples > 0;
            numKeys = wraps ? numSamples + 1 : numSamples;
            for (int ti = 0; ti < trackMap.Length; ti++)
            {
                int b = trackMap[ti].BoneTreeIndex;
                var pos = new JArray(); var rot = new JArray(); var scl = new JArray();
                for (int k = 0; k < numKeys; k++)
                {
                    var t = atoms[ti * numSamples + (k < numSamples ? k : 0)];
                    pos.Add(t.Translation.X); pos.Add(t.Translation.Y); pos.Add(t.Translation.Z);
                    rot.Add(t.Rotation.X); rot.Add(t.Rotation.Y); rot.Add(t.Rotation.Z); rot.Add(t.Rotation.W);
                    scl.Add(t.Scale3D.X); scl.Add(t.Scale3D.Y); scl.Add(t.Scale3D.Z);
                }
                bones.Add(new JObject { ["bone"] = b, ["name"] = b >= 0 && b < numBones ? skeleton.ReferenceSkeleton.FinalRefBoneInfo[b].Name.Text : "", ["pos"] = pos, ["rot"] = rot, ["scl"] = scl });
            }
            if (numKeys == 0) numKeys = 0;
            return Finish(seq, path, skeleton, basePose, bones, numKeys, codec, numSamples, hdr.SampleRate, wraps);
        }
        if (seq.IsValidAdditive()) throw new NotSupportedException("additive sequence with non-ACL codec " + seq.CompressedDataStructure?.GetType().Name);
        codec = seq.CompressedDataStructure?.GetType().Name ?? "raw";
        var set = skeleton.ConvertAnims(seq);
        var cs = set.Sequences[0];
        numKeys = cs.NumFrames;
        for (int b = 0; b < numBones; b++)
        {
            if (seq.FindTrackForBoneIndex(b) < 0) continue;
            var tr = cs.Tracks[b];
            var pos = new JArray(); var rot = new JArray(); var scl = new JArray();
            for (int k = 0; k < numKeys; k++)
            {
                var r = refPose[b].Rotation; var t = refPose[b].Translation; var s = refPose[b].Scale3D;
                tr.GetBoneTransform(k, numKeys, ref r, ref t, ref s);
                pos.Add(t.X); pos.Add(t.Y); pos.Add(t.Z);
                rot.Add(r.X); rot.Add(r.Y); rot.Add(r.Z); rot.Add(r.W);
                scl.Add(s.X); scl.Add(s.Y); scl.Add(s.Z);
            }
            bones.Add(new JObject { ["bone"] = b, ["name"] = skeleton.ReferenceSkeleton.FinalRefBoneInfo[b].Name.Text, ["pos"] = pos, ["rot"] = rot, ["scl"] = scl });
        }
        return Finish(seq, path, skeleton, basePose, bones, numKeys, codec, numKeys, 0, false);
    }

    static JObject Finish(UAnimSequence seq, string path, USkeleton skeleton, FTransform[] basePose, JArray bones, int numKeys, string codec, int numSamples, float sampleRate, bool wraps)
    {
        var curves = new JArray();
        foreach (var c in (seq.CompressedCurveData?.FloatCurves ?? []))
            curves.Add(new JObject { ["name"] = c.CurveName.Text, ["flags"] = c.CurveTypeFlags, ["keys"] = new JArray(c.FloatCurve.Keys.Select(k => new JArray(k.Time, k.Value, k.ArriveTangent, k.LeaveTangent, (int)k.InterpMode))) });
        return new JObject
        {
            ["kind"] = "sequence",
            ["path"] = path,
            ["name"] = seq.Name,
            ["skeleton"] = seq.Skeleton.ToString(),
            ["codec"] = codec,
            ["num_frames"] = seq.NumFrames,
            ["num_keys"] = numKeys,
            ["num_samples"] = numSamples,
            ["sample_rate"] = sampleRate,
            ["wraps"] = wraps,
            ["sequence_length"] = seq.SequenceLength,
            ["rate_scale"] = seq.RateScale,
            ["additive"] = (int)seq.AdditiveAnimType,
            ["ref_pose_type"] = (int)seq.RefPoseType,
            ["ref_pose_seq"] = seq.RefPoseSeq?.GetPathName(),
            ["ref_frame_index"] = seq.RefFrameIndex,
            ["interp"] = (int)seq.Interpolation,
            ["retarget_source"] = seq.RetargetSource.Text,
            ["retarget_pose"] = basePose == null ? null : new JArray(basePose.Select(t => new JObject { ["t"] = V(t.Translation), ["r"] = Q(t.Rotation), ["s"] = V(t.Scale3D) })),
            ["bones"] = bones,
            ["curves"] = curves,
            ["props"] = JToken.Parse(JsonConvert.SerializeObject(seq)),
        };
    }
}
