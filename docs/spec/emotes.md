# Emotes (Battle Royale, 1.8): spec for an implementer

Written 2026-10-05. Every value below was read from the 1.8 cooked data (`fn-inspect props / disasm / classes`), the 1.8 exe decompile
(addresses are VAs in `FortniteClient-Win64-Shipping.exe`, Ghidra names from `C:\FortniteRef\ghidra-project\names.tsv`), the 1.8 SDK dump
(`C:\FortniteRef\Fortnite_1.8.0_SDK`), or UE 4.16 source. Anything not confirmed is marked **UNCONFIRMED**. This covers the BR (Athena) path only.
Save the World paths are mentioned only where BR falls back to them.

---

## 1. What exists in 1.8 BR

| asset | class | `Animation` (montage) | display name |
|---|---|---|---|
| `/Game/Athena/Items/Cosmetics/Dances/EID_DanceMoves` | `AthenaDanceItemDefinition` | `/Game/Animation/Game/MainPlayer/Montages/Emotes/Emote_DanceMoves` | "Dance Moves" |
| `/Game/Athena/Items/Cosmetics/VictoryPoses/EID_RideThePony` | `AthenaVictoryPoseItemDefinition` | `.../Montages/Emotes/Emote_RideThePony` | "Ride the Pony" |
| `/Game/Athena/Items/Cosmetics/ConsumableEmotes/EID_GunShow` | `AthenaConsumableEmoteItemDefinition` | `/Game/Animation/Game/MainPlayer/Emotes/Emote_Flex_CMM_M` | "GUN SHOW" |
| `/Game/Items/Emotes/Dance` (STW) | `FortEmoteItemDefinition`, `CommandName` "Dance" | `Emote_DanceMoves` | "Dance" |
| `/Game/Items/Emotes/RideThePony` (STW) | `FortEmoteItemDefinition`, `CommandName` "RideThePony" | `Emote_RideThePony` | "RideThePony" |

All of these derive from `UFortMontageItemDefinitionBase` (it has the `Animation` soft pointer). Icons: `SmallPreviewImage` / `LargePreviewImage`
(`/Game/Athena/Textures/Icons/Dances/Icon-DanceMoves`, `.../VictoryPoses/Icon-RideThePony`, `.../ConsumableEmotes/Icon-GunShow`).

The default dance comes from the data: `AthenaGameData.FallbackDance` = `EID_DanceMoves` (SDK: `TAssetPtr<UAthenaDanceItemDefinition> FallbackDance` at
+0x40 of the Athena game data). `FFortAthenaLoadout.Dance` (+0x58) is the player's equipped dance. `Athena_PlayerController` CDO's
`CustomizationLoadout` only sets Glider and Pickaxe, so `Dance` is null and the fallback applies. That means a fresh player emotes `EID_DanceMoves`.

---

## 2. Input -> picker (client)

### 2.1 Which input action

There is no "Emote" action in 1.8. BR reuses the **`SocialPicker`** action:

* `AFortPlayerController::SetupInputComponent` (14084a570) binds `SocialPicker` / `ChatPicker` / `NotePicker` (pressed) to vcall thunks
  1408121fc / 140812208 / 140812214 (`jmp [vtbl+0x14a8 / 0x14b0 / 0x14b8]` = slots 661 / 662 / 663).
* In the base class, slots 661/662/663 (`14083ced0`, `140836850`, `14083b210`) broadcast the controller's picker delegate (+0xa60) with mode 2 / 3 / 4.
* `AFortPlayerControllerAthena` overrides **slot 661** with `140443920` (`jmp [vtbl+0x1960]`), which forwards to **slot 812** =
  `AFortPlayerControllerAthena::vfunc_812` (14044d1b0). This is the BR quick emote. `AFortPlayerControllerAthena::SetupInputComponent` (14044c0e0)
  binds nothing emote-related of its own.

Default keys: the gamepad mapping screen (`/Game/UI/Frontend/Settings/GamepadMappingInfo`, `HandleConfigChanged_Athena`) labels **D-pad Down
"Emote"** in all three Athena configs. The keyboard key is **UNCONFIRMED**: `DefaultInput.ini` is encrypted, and `Fort.InputOptions` in `Game.locres` has no
`SocialPicker` row, so 1.8's KBM options screen did not list it as rebindable. `fn-settings` should add an `ActionDef` named `SocialPicker`, labelled "Emote",
with an ASSUMED default key (`B` is the later-BR convention, flagged ASSUMED).

### 2.2 `AFortPlayerControllerAthena::vfunc_812` (14044d1b0)

```
now = GetWorld()->TimeSeconds                       // (*vtbl+0x130)() -> UWorld, +0x908
if (now - LastEmoteTime) > EmoteCooldown:           // strictly greater
    if this->vfunc_0x1360(GameData + 0x2c8):        // gate, see below
        LastEmoteTime = now
        ShowPicker(5)                               // FUN_14084d3b0
```

* `EmoteCooldown` (+0x28D0) = **3.0 s**. `AFortPlayerControllerAthena` ctor 140413a60 sets `param_1[0x51a] = 0x40400000`. The CDO does not override it.
* `LastEmoteTime` (+0x28D4) is stamped when the picker **opens**, not when an emote plays.
* The gate `vfunc_0x1360(GameData+0x2c8)` is **UNCONFIRMED**. `DefaultGameData.EmoteGameplayAbility` = `GAB_Emote_Generic_C`, which makes "can activate
  the emote ability" the likely meaning. Port it as: "the emote ability is not blocked" (see section 4.3).
* `FUN_14084d3b0(mode)`: only acts if `bUsePickers` (+0x21E8 bit 3). It calls two pawn vfuncs (+0x980, +0x938, both with 0; **UNCONFIRMED**, likely
  stop-fire / stop-targeting), then broadcasts the picker delegate (+0xa60) with `mode`.

### 2.3 Picker delegate handler -> emote list

`UFortPickerContext` binds the controller delegate +0xa60 to `FUN_142e1cd30` (in `FUN_142e42670`). For case 5 it calls `FUN_142e52160`:

1. If the controller is not an `AFortPlayerControllerAthena` (`FUN_140378c10` fails): `PickerMode = 8`, option 2 = the STW social -> emote list
   (`FortPickers.EmoteOptions`, see 2.5). BR never takes this path.
2. Otherwise build the list (`FUN_142e069d0`) into `context+0xa0` (a TArray of `FItemDefOptionData`, 0xB0 bytes each):
   * entry 0 = the profile loadout's `Dance` (`+0x58` of the Athena loadout, resolved through `FUN_1404341b0` / `FUN_14041b610`). If that is null,
     use `AthenaGameData.FallbackDance` (`FUN_14041b610`: `if (loadout[0xb] == 0) loadout[0xb] = LoadSynchronous(GameData+0x40)`).
   * then every `AthenaConsumableEmoteItemDefinition` in the account inventory, with its count.
   * truncated to **8 entries** (`min(n, 8)`).
   * Label = item `DisplayName`, or for consumables with a count, `"{0} (x{1})"` (loc key `PickerContext.ConsumableEmoteShortNameWithQty`).
     Brush = the item's small preview image (vfunc +0x2c8). `ItemDef` = the definition.
3. **If the list has exactly 1 entry, play it immediately and close the picker. No radial is shown** (`ServerPlayEmoteItem(list[0].ItemDef)`,
   vtbl +0x16c8, then `FUN_142e0c8a0` = close). A default 1.8 player only has the fallback dance, so pressing the emote key starts Dance Moves directly.
4. Otherwise `PickerMode = 9` and `OnShowPicker(9, -1, false)` is broadcast (`FUN_142e460b0`). `AthenaHUD.ShowPicker` -> `RadialPicker.InitializePicker(9, -1)`.

### 2.4 Accepting / cancelling (mode 9)

`UFortPickerContext::PickerOptionAccepted` (142e46370), case 9: if `0 <= option < count`, call `ServerPlayEmoteItem(list[option].ItemDef)` on the controller
(vtbl +0x16c8), then close. `PickerCanceled` with mode 9 just closes. Close (`FUN_142e0c8a0` -> `FUN_14083b3e0`) clears `bPickerOpen` and restores input.

Picker input component `PickerInputComponent0` (bound in base `SetupInputComponent`): `PickerConfirm`, `PickerCancel`, `PickerNull`, `PickerNextOption` (+1),
`PickerPreviousOption` (-1), `PickerOption1..8`. Each handler only acts when `bUsePickers` (and for cancel, also `bPickerOpen`) is set.

`bRadialClosesOnRelease` (`UFortPlayerInputSettings` +0x6F, Config) is read through `DoesRadialCloseOnRelease`. Its default is in an encrypted ini:
**UNCONFIRMED**, port as false (press to open, click/confirm to choose).

### 2.5 STW emote commands (reference only)

`/Game/UI/InGame/HUD/FortPickers.EmoteOptions` = `[{Label "Dance", EmoteCommand "Dance"}, {Label "RideThePony", EmoteCommand "RideThePony"}]`.
Accepting one in mode 8 (option group 2) calls `FUN_142e46bc0` -> `ServerEmote(FName)` (vtbl +0x16d0). BR does not use this path.

---

## 3. Radial picker widget (`/Game/UI/InGame/HUD/RadialPicker`)

Hosted by `AthenaHUD` as `RadialPicker` (OverlaySlot, centered, initially Collapsed). The CDO has `Visibility = HitTestInvisible`, `ActiveOption = -1`, and these sounds:
`SoundOnSelect` = `Fortnite_UI_MainMenu_Tab_Hover_02_Cue`, `SoundOnAccept` = `Fortnite_UI_MainMenu_Generic_03_Cue`, `SoundOnCancel` = `fort_ui_dismiss_03_Cue`.

Tree (root `Overlay_0`, children in paint order):

1. `Image_2`: `UI_Radar_Circle`, 280x280, centered.
2. `Image_1`: `Fort_Traits/Circle`, 244x244, tint (0,0,0,0.7), centered.
3. `Image_Radial`: `MI-PieMenu` material, fill, ColorAndOpacity alpha 0.85.
4. `ImageRadialPointer`: `CodeReferenced/Radial_Pointer`, centered, starts Hidden, RenderTransform angle -90.
5. `CommonBorder_0` (`M-Border-MainL-Black`, zero padding) -> `SizeBox_0` 700x700 -> `CanvasPanel_0` with `PickerOption0..7` (`RadialPickerItem_C`,
   anchors/alignment 0.5, auto size). The designer offsets are overwritten at runtime.
6. Centre `SizeBox_5` (W 230, H 190, min 230x230) -> VerticalBox: [`KeybindWidget_81` (BoundAction `PickerConfirm`, 30x40) + `Text_OK` "OK"],
   spacer, dotted rule (`Rule-Dotted-HUD`, 8x10 tiled horizontally, padding 10 L/R), spacer, [`KeybindWidget` (BoundAction `PickerCancel`) + "Cancel"].
   Text style is `TextStyle-Base-XS-B-S`, centered.

`RadialPickerItem`: VerticalBox [KeybindWidget, `ImageOrItemSwitcher`]. Switcher page 0 holds the image (24x24 brush, alpha 0.6) and `TextLabel`
(NotoSans Bold 16, centered). Page 1 holds the item card. Mode 9 uses page 0 (`SetShowImageAndLabel(true)` + `SetImageAndLabelContent(label, brush, enabled, i)`).
Only modes 6 and 10 use item cards.

Behaviour (bytecode):

* `InitializePicker(mode, initial)`: `NumOptions = GetNumPickerOptions()`. Create extra items if more than 8 are needed. For each `i < NumOptions`:
  `pos = GetOptionPosition(i)`, set Visible, fill content. Items `>= NumOptions` are Collapsed. `ClearActiveOption`, `SetActiveOption(initial)` if not -1,
  `SetVisibility(Visible)`, `SetInputMode`, `PickerClosing = false`.
* `GetOptionAngle(i) = 90 - (360/NumOptions) * i` (degrees; option 0 at the top, then clockwise).
* `GetOptionPosition(i) = (cos(a) * 230, sin(a) * -230)` (slate units, from the canvas centre).
* Tick, every frame unless `PickerClosing`: pointer angle = `DegAtan2(-(mouseY - vh/2), mouseX - vw/2)` (gamepad: `DegAtan2(stickY * YAxisMultiplier,
  stickX)`). This is skipped while the gamepad is inside the dead zone (`|stick|^2 <= 0.25`). The first option `i` with `AngleDiff(angle(i), pointer) <= 180/NumOptions`
  becomes active if it changed (`SetActiveOption`, plays `SoundOnSelect`). The pointer is then made Visible and `SetPointerDirection()` runs.
* `GetAngleDifference(a, b)`: `d = |a%360 - b%360|`, `if d > 180: d = 360 - d`.
* `SetPointerDirection`: RenderTransform translation `(cos(p) * 130, sin(p) * -130)`, angle `-p`.
* `SetActiveOption(i)`: `item.SetSelected(true)`, `Text_OK = item.PickerItemText`, OK keybind opacity 1.0. `ClearActiveOption` deselects the item, sets
  `ActiveOption = -1`, sets `Text_OK` to empty and OK opacity 0.5.
* Confirm (`OnPickerConfirm`): if `CanConfirm` (active != -1 and item enabled), then `PickerClosing = true`, play `SoundOnAccept`, `ClosePicker`,
  `PickerOptionAccepted(ActiveOption)`. Cancel: `PickerClosing = true`, play `SoundOnCancel`, `ClosePicker`, `PickerCanceled()`.
* `ClosePicker`: Collapsed, `ResetInput` (restores look/move ignore), OK text cleared, active item deselected.
* `SetInputMode`: mouse means show the cursor centered (`SetMouseLocation(vw/2, vh/2)`) and `SetIgnoreLookInput(true)`. Gamepad means hide the cursor; the
  right stick (`YAxisMultiplier = -1`) unless `UseLeftThumbstick()` (then left stick, `YAxisMultiplier = 1`, `SetIgnoreMoveInput(true)`).
* `MoveActiveOption(dir)` (next/prev keys): wraps within `[0, NumOptions)`, sets the pointer angle to that option, and for mouse also warps the cursor to
  `centre + pos * viewportScale`.

---

## 4. Server: playing the emote

### 4.1 RPCs

`AFortPlayerController::ServerPlayEmoteItem(UFortMontageItemDefinitionBase* EmoteAsset)` (wrapper 140a459e0, exec 140b26170). The 0x16c8 slot is
`PlayEmoteItem`. `ServerEmote(FName)` (140a44a50) is STW only. The server grants/activates `DefaultGameData.EmoteGameplayAbility` = `GAB_Emote_Generic_C`
with the item definition as the ability's **source object**. That is what the ability reads, but the server-side activation code itself is
**UNCONFIRMED** (not traced). For consumable emotes, decrementing the count is also **UNCONFIRMED**.

### 4.2 `GAB_Emote_Generic` (`/Game/Abilities/Emotes/GAB_Emote_Generic`)

CDO: `AbilityTags = [Gameplay.Action.Player.Emote]`, `NetExecutionPolicy = ServerInitiated`. Bytecode:

1. `K2_ActivateAbility`: `def = Cast<FortMontageItemDefinitionBase>(GetCurrentSourceObject())`. If the cast fails or `IsAssetNull(def.Animation)`, nothing
   happens (the ability just stays active; port it as an end). Otherwise call `LoadAsset(def.Animation)` (async).
2. On load: `MontageInfo.MontageToPlay = montage` (the other `MontageInfo` fields keep the ctor defaults: `AnimPlayRate 1.0`,
   `AnimRootMotionTranslationScale 1.0`, section enum 0 = default, `bPlayRandomSection` false). Then `PlayMontageInfoAndWaitForTrigger(self, None, MontageInfo)`,
   bind Triggered / Cancelled / Completed, `ReadyForActivation`.
3. **Completed** -> `K2_EndAbility`. **Cancelled** and **Triggered** -> nothing (the montage keeps playing until Completed, or until the ability is cancelled).
4. `K2_OnEndAbility`: if `pawn.EmoteAudioComp` is valid, `EmoteAudioComp.FadeOut(0.2, 0)`.

UE 4.16 `NotifyAbilityEnded` -> `ClearAnimatingAbility`. Ending the ability does **not** stop the montage: a cancelled emote blends out on its own when the
montage instance is stopped by the next montage on the slot, or by the cancel path (`UFortAbilityTask_PlayMontageWaitTarget::OnDestroy` 1402d5e20
only unbinds delegates). **UNCONFIRMED**: whether Fortnite's cancel path calls `MontageStop`. Port it as: on cancel, `Montage_Stop(montage.BlendOut.BlendTime)`.

### 4.3 What cancels an emote

`DefaultGameData.AbilityTagRelationships`: these ability tags list `Gameplay.Action.Player.Emote` in `AbilityTagsToCancel`, so activating any of them
cancels a running emote:

`Player.Move`, `Player.Jump`, `Player.ChangeEquipment`, `Player.DBNO`, `Player.DBNOAthena`, `Action.Death`, `Action.Stunned`, `Action.KnockedBack`,
`Player.Fall`, `Player.AbilityWeapon` (firing/using a weapon), `Player.Crouch`.

So moving, jumping, crouching, falling, switching slot, or firing ends the emote. Also: `Athena_PlayerController.MovementCancellableActionLeashLength = 75.0`
(cm). **UNCONFIRMED**: whether that leash applies to emotes (it is the movement distance before a "movement-cancellable action" is cancelled). Port it as:
cancel when the pawn moves more than 75 cm from where the emote started, as a backstop to the Move tag.

---

## 5. Montages, notifies, audio

All three montages play on slot **`FullBody`**. The AnimGraph already has `Slot FullBody` after the full-body aim offset (`fn-animgraph/src/graph.rs`,
`plan()`: `full_slot=Self::slot(with_ao,"FullBody",montages)`). Feed it with `Montage{slot:"FullBody", ...}`.

| montage | length | blend in / out | segment | notifies |
|---|---|---|---|---|
| `Emote_DanceMoves` | 6.8 s | 0.15 / 0.15, linear (BlendOption not serialized = Linear) | `DanceMoves` 0..6.8, x1 | HolsterWeapon state 0..6.8; PlayFeedbackLine @0.0425; Dance_Clap_Cue @0.3904 and @2.4145; Left/RightFootstep x16 |
| `Emote_RideThePony` | 2.5667 s | 0.25 / 0.25, linear (UE defaults, not serialized) | `Emote_Dance_RideThePony` 0..2.5667, x1 | HolsterWeapon 0..2.5667; PlaySound `fort_dance_music_ridethepony_Cue` @0.0017; PlayFeedbackLine @0.0052 |
| `Emote_Flex_CMM_M` | 8.1333 s | in 0.25 HermiteCubic / out 0.4 HermiteCubic | `Emote_Flex_CMM` 0..8.1333, x1 | HolsterWeapon 0..8.1333 (TrackIndex 1) |

None of the montages loop (`NextSectionName None`, `LoopingCount 1`): emotes end on their own. Footstep notify times for DanceMoves:
R 0.3538, L 0.8924, R 1.3939, L 1.6596, L 1.9127, R 2.1428, L 2.4189, R 2.8849, L 3.1380, R 3.4256, L 3.9778, R 4.2022, L 4.5013, R 5.0190, L 5.5770, R 6.0257.

The `DanceMoves` *sequence* also carries its own notify `FortAnimNotify_PlayEmoteMusic` @0.0316 (`Sound = fort_dance_01_Cue`, `bFollow = true`). UE 4.16
`FAnimMontageInstance::HandleEvents` gathers both montage notifies and segment-sequence notifies (`AnimTrack.GetAnimNotifiesFromTrackPositions`),
so it **fires**. `UFortAnimNotify_PlayEmoteMusic` presumably plays into `pawn.EmoteAudioComp`, the component `GAB_Emote_Generic` fades out
(**UNCONFIRMED** that it writes `EmoteAudioComp`; the name match is the evidence).

* `AnimNotifyState_HolsterWeapon`: Begin -> `pawn.SetHolsterWeapon(true, false)`. End -> `SetHolsterWeapon(false, PlayEquipAnim)` (CDO: `PlayEquipAnim` default false).
  The held weapon/pickaxe is hidden for the whole emote.
* `AnimNotify_PlayFeedbackLine`: `TriggerFeedbackEvent(pawn, Event "Dance_01", ...)`. The bank in the montage (`Soldier_Male_Feedback` /
  `Const_Female_Feedback`) is only the editor reference. At runtime the pawn's own feedback bank resolves `Dance_01`. For example, `Soldier_Male_Feedback`
  maps `Dance_01` to `soldier_m_Dance01_Cue` (Random of 3 dialogue waves `Soldier_m_Dance_010/020/040`, `Fort_VO_Player_Att`, PreventNew).
* Sound cues:
  * `Dance_Clap_Cue`: wave `dance_soldier_clap`, 0.548 s, `Fort_VO_Player_Att`, class `Fort_VO_Players_PSM`, PreventNew.
  * `fort_dance_01_Cue`: Random(`dance_soldier_03`, `outlander_dance01`, `ninja_dance_01`), 7.81 s, class `Music`, MaxCount 1, PreventNew.
  * `fort_dance_music_ridethepony_Cue`: Random(`_A`, `_B`), 4.30 s, class `Fort_11_Base_Music`, MaxCount 1, bLimitToOwner.

---

## 6. Port checklist

1. Settings: add a `SocialPicker` action labelled "Emote" (ASSUMED key B, gamepad D-pad Down).
2. Controller: on press, apply the cooldown check (`now - last > 3.0`) and the gate (emote not blocked), stamp `last`, and build the list
   (loadout dance or `FallbackDance`, plus consumables, max 8).
   With 1 entry, play it now. Otherwise open the radial in mode 9.
3. Radial: implement section 3 (geometry radius 230, pointer radius 130, sector half-width 180/N, dead zone 0.25, sounds, OK/Cancel centre).
4. Ability: load `def.Animation`, play on `FullBody` with the montage's blend in/out, holster the weapon for the HolsterWeapon window, and end on montage completion.
   On end, fade the emote audio over 0.2 s.
5. Cancel on any tag in section 4.3, and on 75 cm drift (ASSUMED).
6. Notifies: footsteps (existing footstep system), clap cue, feedback VO `Dance_01`, emote music (DanceMoves sequence notify; RideThePony
   montage PlaySound).

## 7. Open items (UNCONFIRMED)

* KBM default key for `SocialPicker`, and the `bRadialClosesOnRelease` default (encrypted ini).
* `vfunc_0x1360(GameData+0x2c8)` gate semantics; the pawn vfuncs +0x980 / +0x938 called before opening a picker.
* The server path from `ServerPlayEmoteItem` to activating `GAB_Emote_Generic`, and consumable count decrement.
* Whether an ability cancel stops the montage (UE 4.16 `EndAbility` alone does not).
* Whether `MovementCancellableActionLeashLength` (75) applies to emotes.
* That `FortAnimNotify_PlayEmoteMusic` targets `EmoteAudioComp`.

## Coordinator addendum (2026-10-05): cooked ini evidence
Source: `C:\FortniteRef\scratch\CookedIniVersion.txt`, extracted from the unencrypted pakchunk0 `CookedIniVersion.txt`.
- `bRadialClosesOnRelease=true` in every input preset, including `ConfigD_Athena` (line 2593). This resolves the UNCONFIRMED default: port it as **true**, not false.
- `SocialPicker` is bound **only** to `Gamepad_DPad_Down` in every preset (`ConfigD_Athena` ActionMappings:109, line 2736). No keyboard key for SocialPicker appears anywhere in the cooked ini.
- Do not assume B. Before porting the keyboard key, check the `InputActionDataTable` row, the 1.8 keybind settings widget data and the exe default action mappings. If none of them gives a keyboard key, the 1.8 PC default is unbound, and the port must match that and leave it to user rebinding.

## Port addendum (2026-10-25): resolved items, from the decompile at `D:\Fortnite\FortniteRef\decomp\named` and the paks

* **Keyboard key = B, through `ChatPicker`.** `AFortPlayerControllerAthena` vtable (0x144070898) overrides slot 661 (`SocialPicker`) AND slot 662
  (`ChatPicker`) with the same thunk 140443920 (`mov rax,[rcx]; jmp [rax+0x1960]` = vfunc_812, the emote). Slot 663 (`NotePicker`) is 141a68a80 (`ret 0`).
  `ChatPicker` is the "Quick Chat Message" row of the 1.8 keybinds with default `B` (`fn-settings` table, recovered). So in BR, B (KBM) and D-pad Down
  (gamepad, `SocialPicker`) both run vfunc_812. `PickerCancel` also lists B, but only on the picker's own input component (open picker: B cancels).
  Note: the cooked ini rows quoted above are the `Mac.`-prefixed ones (the only ones `CookedIniVersion.txt` stores).
* **The gate.** `vfunc_812` calls vtbl+0x1360 with `&Tags + 0x2c8`, where `Tags` = `FUN_1405c94c0()` = the native tag table filled by `FUN_1405af6c0`
  (`[0x59]` = `Gameplay.Action.Player.InteractUse`; `[0x5a]` = `...Emote`, `[0x51]` = `...Move`). It is NOT `GameData+0x2c8`. Athena 0x1360 =
  `14041ed80`: if the tag is `AimDownsights` (`[0x5e]`) it checks the weapon, else base `140816400`: pawn (`PC+0x770`) exists, pawn byte +0xf58 == 0,
  then pawn vtbl+0xa90 = `1407d0730` -> `ASC(+0xc40)` -> `14028fb20` = `!AreAbilityTagsBlocked({InteractUse})`. `AbilityTagRelationships` rows whose
  `AbilityTagsToBlock` contain `InteractUse`: Medium, Heavy, DBNO, DBNOAthena, Death, Stunned, KnockedBack, AbilityWeapon, Skydive. Healing
  (`InterruptibleHealingItem`) does not block it.
* **Pawn vfuncs before a picker opens** (`FortPlayerPawnAthena` vtable 0x144072328): +0x980 = `1407f8b80` (clears the fire-pressed byte and releases the
  fire ability input), +0x938 = `1407f8be0` (same for targeting).
* **Server path.** `ServerPlayEmoteItem_Implementation` = slot 428 = `140847700` -> `FUN_1408251c0`: needs the pawn + ASC, repeats the gate, loads
  `GameData+0x1808` (`EmoteGameplayAbility` = `GAB_Emote_Generic_C`), for an `AthenaConsumableEmoteItemDefinition` requires `FUN_140426390` (count > 0,
  then an MCP consume command), then `GiveAbilityAndActivateOnce` (`FUN_143e3c570`) with the item as the spec's source object.
* **Move cancel = the 75 cm leash.** `UFortAbilitySystemComponent` `FUN_14029f800`: when an ability whose tags are in the `Move` relationship's
  `AbilityTagsToCancel` activates (Emote is), it calls PC vtbl+0x13b0 (`StartMovementCancellableAction`, `FUN_14084e5e0`). The Athena PC tick
  (`FUN_14044e880`) applies the `Move` cancel when the pawn is more than `MovementCancellableActionLeashLength` (+0x31dc, 75 cm, 3D) from the leash
  location. Athena's movement-input override (slot 629 = `14044e460`) does NOT apply the immediate base cancel. So walking cancels after 75 cm.
* **No emote camera.** No Athena camera mode, no cooked BP and no native tag use for emotes (the pak scan finds `IsPlayingEmote` only in the STW
  `BP_PlayerSpawnTeleportPad`). The normal third-person camera stays.
* **`bLimitToOwner`** on `fort_dance_music_ridethepony_Cue` is a concurrency setting (MaxCount 1 per owner), not "only the owner hears it".
  `fort_dance_01_Cue` is MaxCount 1, PreventNew (one at a time in the world).
* **Locker.** `AthenaCustomizationTab` has `CharacterSlot`, `PickaxeSlot`, `GliderSlot`, `BannerSlot`: no dance slot in 1.8, so `Loadout.Dance` stays
  null and `FallbackDance` applies. Nothing to port in the lobby.
* Still open: `bRadialClosesOnRelease` (no cooked caller of `DoesRadialCloseOnRelease`, SocialPicker binds pressed only: not ported), the meaning of
  pawn byte +0xf58, the `M-PieMenu` gradient formula (cooked material keeps no expression graph), `PlayFeedbackLine` VO (no feedback banks in the port),
  who plays the victory pose (`Loadout+0x60` reader not found).
