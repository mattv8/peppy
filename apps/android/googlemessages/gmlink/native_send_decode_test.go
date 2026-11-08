package gmlink

import (
	"bytes"
	"encoding/json"
	"io"
	"strings"
	"testing"
)

// schemaDerivedNativeCommitInput is maintained from the reviewed commit schema, not a core fixture.
const schemaDerivedNativeCommitInput = `{"command_id":"schema-command","tmp_id":"schema-tmp","mapping_revision":7,"proof_version":2,"proof_digest":[0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31],"message_body":"schema body","target":{"proof_version":2,"peppy_conversation_id":"peppy-1","google_conversation_id":"google-1","conversation_type":"Rcs","status":"eligible","send_mode":"Automatic","read_only":false,"default_outgoing":true,"participants":[{"identity":"peer-1","google_participant_id":"participant-1","identifier_type":-1,"is_visible":true,"is_me":false}],"class":"OneToOne","canonical_recipient":"recipient-1","canonical_own_identity":"own-1","identity_region":"US","canonicalizer_revision":1,"selected_sim_id":"sim-1","carrier_binding":{"carrier_route_id":"route-1","android_subscription_id":7,"is_current_default_sms":true,"own_number_raw":"own-raw","own_number_source":"SubscriptionManagerPhoneNumber","route_observed_at_ms":1000,"route_context_revision":8,"same_handset_evidence_id":"evidence-1"},"outgoing":{"outgoing_id":"outgoing-1","sim_participant_id":"sim-participant-1","own_identity_raw":"own-raw","sim_payload":{"two":2,"sim_number":7},"rcs_chats_enabled":true},"evidence":{"lifetime":{"runtime_id":"runtime-1","run_generation":9,"link_generation":10,"session_generation":11,"context_revision":12},"lookup_id":"lookup-1","lookup_complete":true,"google_candidate_count":1,"settings_revision":13,"settings_provenance":"CurrentEvent","settings_observed_at_ms":1001,"settings_stream_epoch":14,"settings_currentness":"ContinuousVerifiedFeed","current_settings_sims":[{"outgoing_id":"current-outgoing-1","sim_participant_id":"current-participant-1","own_identity_raw":"current-own-raw","sim_payload":{"two":3,"sim_number":8},"rcs_chats_enabled":false}],"settings_validated_at_ms":1002,"conversation_observed_at_ms":1003}}}`

func nativeCommitFixture(t *testing.T) string { t.Helper(); return schemaDerivedNativeCommitInput }

func TestNativeRcsDecodeCommitFixturePreservesTargetAndEvidence(t *testing.T) {
	fixture := nativeCommitFixture(t)
	commit, ok := decodeNativeRcsCommit(fixture)
	if !ok {
		t.Fatal("real Rust commit fixture was rejected")
	}
	if commit.CommandID != "schema-command" || commit.MessageBody != "schema body" || commit.ProofDigest[31] != 31 {
		t.Fatalf("commit fields = %#v", commit)
	}
	if commit.Target.GoogleConversationID != "google-1" || commit.Target.CarrierBinding.AndroidSubscriptionID != 7 || commit.Target.Participants[0].IdentifierType != -1 {
		t.Fatalf("target was not preserved: %#v", commit.Target)
	}
	if commit.Target.Evidence.Lifetime.RuntimeID != "runtime-1" || commit.Target.Evidence.SettingsStreamEpoch != 14 || commit.Target.Evidence.CurrentSettingsSIMs[0].SIMPayload.SIMNumber != 8 {
		t.Fatalf("evidence was not preserved: %#v", commit.Target.Evidence)
	}
	assertNativeCommitMatchesGolden(t, parseNativeGolden(t, fixture), commit)
}

func TestNativeRcsDecodeCommitRejectsStrictViolations(t *testing.T) {
	fixture := nativeCommitFixture(t)
	cases := map[string]string{
		"unknown nested":      strings.Replace(fixture, `"runtime_id":"runtime-1"`, `"runtime_id":"runtime-1","unknown":true`, 1),
		"duplicate nested":    strings.Replace(fixture, `"runtime_id":"runtime-1"`, `"runtime_id":"runtime-1","runtime_id":"again"`, 1),
		"missing nested":      strings.Replace(fixture, `"runtime_id":"runtime-1",`, "", 1),
		"null nested":         strings.Replace(fixture, `"runtime_id":"runtime-1"`, `"runtime_id":null`, 1),
		"case alias":          strings.Replace(fixture, `"command_id"`, `"Command_ID"`, 1),
		"short digest":        strings.Replace(fixture, `,31]`, `]`, 1),
		"digest range":        strings.Replace(fixture, `[0,`, `[256,`, 1),
		"unsigned overflow":   strings.Replace(fixture, `"mapping_revision":7`, `"mapping_revision":18446744073709551616`, 1),
		"float":               strings.Replace(fixture, `"mapping_revision":7`, `"mapping_revision":7.0`, 1),
		"exponent":            strings.Replace(fixture, `"mapping_revision":7`, `"mapping_revision":7e0`, 1),
		"unsupported version": strings.Replace(fixture, `"proof_version":2`, `"proof_version":3`, 1),
		"trailing token":      fixture + " true",
		"trailing null":       fixture + " null",
	}
	for name, input := range cases {
		t.Run(name, func(t *testing.T) {
			if _, ok := decodeNativeRcsCommit(input); ok {
				t.Fatal("accepted invalid input")
			}
		})
	}
}

func TestNativeRcsDecodeCommitBodyAndCapBounds(t *testing.T) {
	fixture := nativeCommitFixture(t)
	for _, body := range []string{"", strings.Repeat("a", 4096), strings.Repeat("é", 2048), strings.Repeat("a", 4097)} {
		input := strings.Replace(fixture, `"message_body":"schema body"`, `"message_body":"`+body+`"`, 1)
		_, ok := decodeNativeRcsCommit(input)
		if want := len(body) > 0 && len(body) <= 4096; ok != want {
			t.Fatalf("body length %d accepted=%t want=%t", len(body), ok, want)
		}
	}
	padded := fixture + strings.Repeat(" ", 131072-len(fixture))
	if _, ok := decodeNativeRcsCommit(padded); !ok {
		t.Fatal("rejected exact-cap valid JSON")
	}
	if _, ok := decodeNativeRcsCommit(padded + " "); ok {
		t.Fatal("accepted over-cap input")
	}
}

func TestNativeRcsDecodePrepareIsRecursiveAndStrict(t *testing.T) {
	valid := `{"command_id":"command","canonicalizer_revision":1,"identity_region":"US","carrier_binding":{"carrier_route_id":"android-subscription-7","android_subscription_id":7,"own_number_raw":"+12025550100","own_number_source":"SubscriptionManagerPhoneNumber","route_observed_at_ms":-1}}`
	request, ok := decodeNativeRcsPrepare(valid)
	if !ok || request.CarrierBinding.RouteObservedAtMS != -1 || request.CanonicalizerRevision != 1 {
		t.Fatalf("prepare = %#v, %t", request, ok)
	}
	for _, input := range []string{
		strings.Replace(valid, `"route_observed_at_ms":-1`, `"route_observed_at_ms":-1,"unknown":true`, 1),
		strings.Replace(valid, `"identity_region":"US"`, `"identity_region":null`, 1),
		strings.Replace(valid, `"canonicalizer_revision":1`, `"canonicalizer_revision":1e0`, 1),
		strings.Replace(valid, `"identity_region":"US"`, `"identity_region":"\ud800"`, 1),
		valid + "\xff",
	} {
		if _, ok := decodeNativeRcsPrepare(input); ok {
			t.Fatal("accepted invalid prepare input")
		}
	}
}

func TestNativeRcsDecodeCommitIntegerBoundaries(t *testing.T) {
	fixture := nativeCommitFixture(t)
	for _, input := range []string{
		strings.Replace(fixture, `"mapping_revision":7`, `"mapping_revision":18446744073709551615`, 1),
		strings.Replace(fixture, `"route_observed_at_ms":1000`, `"route_observed_at_ms":-9223372036854775808`, 1),
		strings.Replace(fixture, `"route_observed_at_ms":1000`, `"route_observed_at_ms":9223372036854775807`, 1),
		strings.Replace(fixture, `"android_subscription_id":7`, `"android_subscription_id":-2147483648`, 1),
		strings.Replace(fixture, `"android_subscription_id":7`, `"android_subscription_id":2147483647`, 1),
	} {
		if _, ok := decodeNativeRcsCommit(input); !ok {
			t.Fatal("rejected in-range integer")
		}
	}
	for _, input := range []string{
		strings.Replace(fixture, `"route_observed_at_ms":1000`, `"route_observed_at_ms":9223372036854775808`, 1),
		strings.Replace(fixture, `"android_subscription_id":7`, `"android_subscription_id":2147483648`, 1),
	} {
		if _, ok := decodeNativeRcsCommit(input); ok {
			t.Fatal("accepted out-of-range integer")
		}
	}
}

func TestNativeRcsDecodeRejectsInvalidNestedArrayAndSurrogates(t *testing.T) {
	fixture := nativeCommitFixture(t)
	currentSettingsSIMs := `"current_settings_sims":[{"outgoing_id":"current-outgoing-1","sim_participant_id":"current-participant-1","own_identity_raw":"current-own-raw","sim_payload":{"two":3,"sim_number":8},"rcs_chats_enabled":false}]`
	for _, input := range []string{
		strings.Replace(fixture, currentSettingsSIMs, `"current_settings_sims":null`, 1),
		strings.Replace(fixture, currentSettingsSIMs, `"current_settings_sims":{}`, 1),
		strings.Replace(fixture, currentSettingsSIMs, `"current_settings_sims":false`, 1),
		strings.Replace(fixture, currentSettingsSIMs, `"current_settings_sims":[null]`, 1),
		strings.Replace(fixture, `"runtime_id":"runtime-1"`, `"runtime_id":"\ud800"`, 1),
		strings.Replace(fixture, `"runtime_id":"runtime-1"`, `"runtime_id":"\udc00"`, 1),
		strings.Replace(fixture, `"runtime_id":"runtime-1"`, `"runtime_id":"\ud800\u1234"`, 1),
	} {
		if _, ok := decodeNativeRcsCommit(input); ok {
			t.Fatal("accepted invalid nested array or surrogate")
		}
	}
}

func TestNativeRcsDecodePreservesStringsAndOtherEnums(t *testing.T) {
	fixture := nativeCommitFixture(t)
	for _, input := range []string{
		strings.Replace(fixture, `"runtime_id":"runtime-1"`, `"runtime_id":"\ud83d\ude00"`, 1),
		strings.Replace(fixture, `"runtime_id":"runtime-1"`, `"runtime_id":"�"`, 1),
		strings.Replace(strings.Replace(strings.Replace(fixture, `"conversation_type":"Rcs"`, `"conversation_type":"Other"`, 1), `"send_mode":"Automatic"`, `"send_mode":"Other"`, 1), `"class":"OneToOne"`, `"class":"SelfChat"`, 1),
		strings.Replace(fixture, `"tmp_id":"schema-tmp"`, `"tmp_id":"`+strings.Repeat("t", 256)+`"`, 1),
		strings.Replace(fixture, `"canonicalizer_revision":1`, `"canonicalizer_revision":4294967295`, 1),
	} {
		if _, ok := decodeNativeRcsCommit(input); !ok {
			t.Fatal("rejected schema-valid string or enum")
		}
	}
	pair := strings.Replace(fixture, `"runtime_id":"runtime-1"`, `"runtime_id":"\ud83d\ude00"`, 1)
	commit, ok := decodeNativeRcsCommit(pair)
	if !ok || commit.Target.Evidence.Lifetime.RuntimeID != "😀" {
		t.Fatalf("surrogate pair was not decoded exactly: %#v, %t", commit.Target.Evidence.Lifetime, ok)
	}
	replacement := strings.Replace(fixture, `"runtime_id":"runtime-1"`, `"runtime_id":"�"`, 1)
	commit, ok = decodeNativeRcsCommit(replacement)
	if !ok || commit.Target.Evidence.Lifetime.RuntimeID != "�" {
		t.Fatalf("literal replacement character was not preserved: %#v, %t", commit.Target.Evidence.Lifetime, ok)
	}
	if _, ok := decodeNativeRcsCommit(strings.Replace(fixture, `"tmp_id":"schema-tmp"`, `"tmp_id":"`+strings.Repeat("t", 257)+`"`, 1)); ok {
		t.Fatal("accepted oversized tmp_id")
	}
}

func parseNativeGolden(t *testing.T, input string) map[string]any {
	t.Helper()
	decoder := json.NewDecoder(bytes.NewBufferString(input))
	decoder.UseNumber()
	var golden map[string]any
	if err := decoder.Decode(&golden); err != nil {
		t.Fatal(err)
	}
	if _, err := decoder.Token(); err != io.EOF {
		t.Fatalf("unexpected trailing golden token: %v", err)
	}
	return golden
}

func assertNativeCommitMatchesGolden(t *testing.T, golden map[string]any, commit nativeRcsCommitProjection) {
	t.Helper()
	assertGoldenString(t, golden, "command_id", commit.CommandID)
	assertGoldenString(t, golden, "tmp_id", commit.TmpID)
	assertGoldenUint64(t, golden, "mapping_revision", commit.MappingRevision)
	assertGoldenUint32(t, golden, "proof_version", commit.ProofVersion)
	assertGoldenString(t, golden, "message_body", commit.MessageBody)
	for index, value := range golden["proof_digest"].([]any) {
		if commit.ProofDigest[index] != byte(goldenNumber(t, value)) {
			t.Fatalf("proof_digest[%d] changed", index)
		}
	}
	assertNativeTargetMatchesGolden(t, golden["target"].(map[string]any), commit.Target)
}

func assertNativeTargetMatchesGolden(t *testing.T, golden map[string]any, target nativeRcsTargetSnapshot) {
	t.Helper()
	assertGoldenUint32(t, golden, "proof_version", target.ProofVersion)
	for key, actual := range map[string]string{"peppy_conversation_id": target.PeppyConversationID, "google_conversation_id": target.GoogleConversationID, "conversation_type": target.ConversationType, "status": target.Status, "send_mode": target.SendMode, "class": target.Class, "canonical_recipient": target.CanonicalRecipient, "canonical_own_identity": target.CanonicalOwnIdentity, "identity_region": target.IdentityRegion, "selected_sim_id": target.SelectedSIMID} {
		assertGoldenString(t, golden, key, actual)
	}
	assertGoldenBool(t, golden, "read_only", target.ReadOnly)
	assertGoldenBool(t, golden, "default_outgoing", target.DefaultOutgoing)
	assertGoldenUint32(t, golden, "canonicalizer_revision", target.CanonicalizerRevision)
	participant := golden["participants"].([]any)[0].(map[string]any)
	actualParticipant := target.Participants[0]
	assertGoldenString(t, participant, "identity", actualParticipant.Identity)
	assertGoldenString(t, participant, "google_participant_id", actualParticipant.GoogleParticipantID)
	assertGoldenInt32(t, participant, "identifier_type", actualParticipant.IdentifierType)
	assertGoldenBool(t, participant, "is_visible", actualParticipant.IsVisible)
	assertGoldenBool(t, participant, "is_me", actualParticipant.IsMe)
	assertNativeBindingMatchesGolden(t, golden["carrier_binding"].(map[string]any), target.CarrierBinding)
	assertNativeOutgoingMatchesGolden(t, golden["outgoing"].(map[string]any), target.Outgoing)
	assertNativeEvidenceMatchesGolden(t, golden["evidence"].(map[string]any), target.Evidence)
}

func assertNativeBindingMatchesGolden(t *testing.T, golden map[string]any, binding nativeRcsCarrierBinding) {
	for key, actual := range map[string]string{"carrier_route_id": binding.CarrierRouteID, "own_number_raw": binding.OwnNumberRaw, "own_number_source": binding.OwnNumberSource, "same_handset_evidence_id": binding.SameHandsetEvidenceID} {
		assertGoldenString(t, golden, key, actual)
	}
	assertGoldenInt32(t, golden, "android_subscription_id", binding.AndroidSubscriptionID)
	assertGoldenBool(t, golden, "is_current_default_sms", binding.IsCurrentDefaultSMS)
	assertGoldenInt64(t, golden, "route_observed_at_ms", binding.RouteObservedAtMS)
	assertGoldenUint64(t, golden, "route_context_revision", binding.RouteContextRevision)
}

func assertNativeOutgoingMatchesGolden(t *testing.T, golden map[string]any, outgoing nativeRcsOutgoing) {
	for key, actual := range map[string]string{"outgoing_id": outgoing.OutgoingID, "sim_participant_id": outgoing.SIMParticipantID, "own_identity_raw": outgoing.OwnIdentityRaw} {
		assertGoldenString(t, golden, key, actual)
	}
	payload := golden["sim_payload"].(map[string]any)
	assertGoldenInt32(t, payload, "two", outgoing.SIMPayload.Two)
	assertGoldenInt32(t, payload, "sim_number", outgoing.SIMPayload.SIMNumber)
	assertGoldenBool(t, golden, "rcs_chats_enabled", outgoing.RcsChatsEnabled)
}

func assertNativeEvidenceMatchesGolden(t *testing.T, golden map[string]any, evidence nativeRcsEvidence) {
	lifetime := golden["lifetime"].(map[string]any)
	assertGoldenString(t, lifetime, "runtime_id", evidence.Lifetime.RuntimeID)
	assertGoldenUint64(t, lifetime, "run_generation", evidence.Lifetime.RunGeneration)
	assertGoldenUint64(t, lifetime, "link_generation", evidence.Lifetime.LinkGeneration)
	assertGoldenUint64(t, lifetime, "session_generation", evidence.Lifetime.SessionGeneration)
	assertGoldenUint64(t, lifetime, "context_revision", evidence.Lifetime.ContextRevision)
	assertGoldenString(t, golden, "lookup_id", evidence.LookupID)
	assertGoldenBool(t, golden, "lookup_complete", evidence.LookupComplete)
	assertGoldenUint32(t, golden, "google_candidate_count", evidence.GoogleCandidateCount)
	assertGoldenUint64(t, golden, "settings_revision", evidence.SettingsRevision)
	assertGoldenString(t, golden, "settings_provenance", evidence.SettingsProvenance)
	assertGoldenInt64(t, golden, "settings_observed_at_ms", evidence.SettingsObservedAtMS)
	assertGoldenUint64(t, golden, "settings_stream_epoch", evidence.SettingsStreamEpoch)
	assertGoldenString(t, golden, "settings_currentness", evidence.SettingsCurrentness)
	assertGoldenInt64(t, golden, "settings_validated_at_ms", evidence.SettingsValidatedAtMS)
	assertGoldenInt64(t, golden, "conversation_observed_at_ms", evidence.ConversationObservedAtMS)
	assertNativeOutgoingMatchesGolden(t, golden["current_settings_sims"].([]any)[0].(map[string]any), evidence.CurrentSettingsSIMs[0])
}

func goldenNumber(t *testing.T, value any) int64 {
	t.Helper()
	number, ok := value.(json.Number)
	if !ok {
		t.Fatalf("golden value %T is not a number", value)
	}
	parsed, err := number.Int64()
	if err != nil {
		t.Fatal(err)
	}
	return parsed
}
func assertGoldenString(t *testing.T, golden map[string]any, key, actual string) {
	t.Helper()
	if expected := golden[key].(string); actual != expected {
		t.Fatalf("%s = %q, want %q", key, actual, expected)
	}
}
func assertGoldenBool(t *testing.T, golden map[string]any, key string, actual bool) {
	t.Helper()
	if expected := golden[key].(bool); actual != expected {
		t.Fatalf("%s = %t, want %t", key, actual, expected)
	}
}
func assertGoldenUint64(t *testing.T, golden map[string]any, key string, actual uint64) {
	t.Helper()
	if expected := uint64(goldenNumber(t, golden[key])); actual != expected {
		t.Fatalf("%s = %d, want %d", key, actual, expected)
	}
}
func assertGoldenUint32(t *testing.T, golden map[string]any, key string, actual uint32) {
	t.Helper()
	if expected := uint32(goldenNumber(t, golden[key])); actual != expected {
		t.Fatalf("%s = %d, want %d", key, actual, expected)
	}
}
func assertGoldenInt64(t *testing.T, golden map[string]any, key string, actual int64) {
	t.Helper()
	if expected := goldenNumber(t, golden[key]); actual != expected {
		t.Fatalf("%s = %d, want %d", key, actual, expected)
	}
}
func assertGoldenInt32(t *testing.T, golden map[string]any, key string, actual int32) {
	t.Helper()
	if expected := int32(goldenNumber(t, golden[key])); actual != expected {
		t.Fatalf("%s = %d, want %d", key, actual, expected)
	}
}
