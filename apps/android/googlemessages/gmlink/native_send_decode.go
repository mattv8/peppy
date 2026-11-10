package gmlink

import (
	"bytes"
	"encoding/base64"
	"encoding/json"
	"io"
	"math"
	"strconv"
	"unicode/utf8"
)

const maxNativeRcsCommitJSONBytes = 131072
const maxNativeRcsFactsJSONBytes = 65536

func decodeNativeRcsFacts(input string) (nativeRcsFacts, bool) {
	if len(input) > maxNativeRcsFactsJSONBytes || !utf8.ValidString(input) {
		return nativeRcsFacts{}, false
	}
	values, ok := nativeObject([]byte(input), "request", "stored_mapping")
	request, requestOK := decodeNativeRcsFactsRequest(values["request"])
	mapping, mappingOK := decodeNativeRcsStoredMapping(values["stored_mapping"])
	return nativeRcsFacts{Request: request, StoredMapping: mapping}, ok && requestOK && mappingOK
}

func decodeNativeRcsFactsRequest(raw json.RawMessage) (nativeRcsFactsRequest, bool) {
	values, ok := nativeObject(raw, "command_id", "peppy_conversation_id", "canonical_recipient")
	request := nativeRcsFactsRequest{CommandID: nativeString(values["command_id"]), PeppyConversationID: nativeString(values["peppy_conversation_id"]), CanonicalRecipient: nativeString(values["canonical_recipient"])}
	return request, ok && nativeKinds(values, []string{"command_id", "peppy_conversation_id", "canonical_recipient"}, nil, nil, nil) && request.CommandID != "" && request.PeppyConversationID != "" && request.CanonicalRecipient != ""
}

func decodeNativeRcsStoredMapping(raw json.RawMessage) (nativeRcsStoredMapping, bool) {
	values, ok := nativeObject(raw, "conversation_id", "google_conversation_id", "class", "proof_version", "proof", "mapping_revision", "verified_at_ms")
	proof, proofOK := decodeNativeRcsStoredProof(values["proof"])
	mapping := nativeRcsStoredMapping{ConversationID: nativeString(values["conversation_id"]), GoogleConversationID: nativeString(values["google_conversation_id"]), Class: nativeString(values["class"]), ProofVersion: nativeUint32(values["proof_version"]), Proof: proof, MappingRevision: nativeUint64(values["mapping_revision"]), VerifiedAtMS: nativeInt64(values["verified_at_ms"])}
	return mapping, ok && proofOK && nativeKinds(values, []string{"conversation_id", "google_conversation_id", "class"}, nil, []string{"proof_version", "mapping_revision"}, []string{"verified_at_ms"}) && nativeUint32Valid(values["proof_version"]) && validEnum(mapping.Class, "OneToOne", "SelfChat", "Group") && mapping.ProofVersion == 2
}

func decodeNativeRcsStoredProof(raw json.RawMessage) (nativeRcsStoredProof, bool) {
	values, ok := nativeObject(raw, "canonical_participant", "identity_region", "own_identity", "digest")
	digest, digestOK := decodeDigest(values["digest"])
	proof := nativeRcsStoredProof{CanonicalParticipant: nativeString(values["canonical_participant"]), IdentityRegion: nativeString(values["identity_region"]), OwnIdentity: nativeString(values["own_identity"]), Digest: digest}
	return proof, ok && digestOK && nativeKinds(values, []string{"canonical_participant", "identity_region", "own_identity"}, nil, nil, nil) && proof.CanonicalParticipant != "" && proof.IdentityRegion != "" && proof.OwnIdentity != ""
}

func decodeNativeRcsPrepare(input string) (nativeRcsPrepareRequest, bool) {
	values, ok := nativeObject([]byte(input), "command_id", "canonicalizer_revision", "identity_region", "carrier_binding", "expected_member_digest")
	if !ok {
		values, ok = nativeObject([]byte(input), "command_id", "canonicalizer_revision", "identity_region", "carrier_binding")
	}
	if !ok {
		return nativeRcsPrepareRequest{}, false
	}
	_, hasExpectedMemberDigest := values["expected_member_digest"]
	binding, ok := decodePrepareCarrierBinding(values["carrier_binding"])
	request := nativeRcsPrepareRequest{
		CommandID:             nativeString(values["command_id"]),
		CanonicalizerRevision: nativeUint32(values["canonicalizer_revision"]),
		IdentityRegion:        nativeString(values["identity_region"]),
		CarrierBinding:        binding,
		ExpectedMemberDigest:  nativeString(values["expected_member_digest"]),
	}
	strings := []string{"command_id", "identity_region"}
	if request.ExpectedMemberDigest != "" {
		strings = append(strings, "expected_member_digest")
	}
	return request, ok && nativeKinds(values, strings, nil, []string{"canonicalizer_revision"}, nil) && nativeUint32Valid(values["canonicalizer_revision"]) && request.CommandID != "" && request.IdentityRegion != "" && (!hasExpectedMemberDigest || validMemberDigest(request.ExpectedMemberDigest))
}

func decodeNativeRcsCommit(input string) (nativeRcsCommitProjection, bool) {
	if len(input) > maxNativeRcsCommitJSONBytes || !utf8.ValidString(input) {
		return nativeRcsCommitProjection{}, false
	}
	values, ok := nativeObject([]byte(input), "command_id", "tmp_id", "mapping_revision", "proof_version", "proof_digest", "message_body", "target", "expected_member_digest")
	if !ok {
		values, ok = nativeObject([]byte(input), "command_id", "tmp_id", "mapping_revision", "proof_version", "proof_digest", "message_body", "target")
	}
	if !ok {
		return nativeRcsCommitProjection{}, false
	}
	_, hasExpectedMemberDigest := values["expected_member_digest"]
	target, targetOK := decodeTarget(values["target"])
	digest, digestOK := decodeDigest(values["proof_digest"])
	commit := nativeRcsCommitProjection{
		CommandID:            nativeString(values["command_id"]),
		TmpID:                nativeString(values["tmp_id"]),
		MappingRevision:      nativeUint64(values["mapping_revision"]),
		ProofVersion:         nativeUint32(values["proof_version"]),
		ProofDigest:          digest,
		MessageBody:          nativeString(values["message_body"]),
		Target:               target,
		ExpectedMemberDigest: nativeString(values["expected_member_digest"]),
	}
	strings := []string{"command_id", "tmp_id", "message_body"}
	if commit.ExpectedMemberDigest != "" {
		strings = append(strings, "expected_member_digest")
	}
	return commit, targetOK && digestOK && nativeKinds(values, strings, nil, []string{"mapping_revision", "proof_version"}, nil) && commit.CommandID != "" && validNativeTmpID(commit.TmpID) && validNativeBody(commit.MessageBody) && commit.ProofVersion == 2 && commit.Target.ProofVersion == 2 && (!hasExpectedMemberDigest || validMemberDigest(commit.ExpectedMemberDigest))
}

func validMemberDigest(value string) bool {
	digest, err := base64.StdEncoding.DecodeString(value)
	return err == nil && len(digest) == 32
}

func decodePrepareCarrierBinding(raw json.RawMessage) (nativeRcsPrepareCarrierBinding, bool) {
	v, ok := nativeObject(raw, "carrier_route_id", "android_subscription_id", "own_number_raw", "own_number_source", "route_observed_at_ms")
	b := nativeRcsPrepareCarrierBinding{nativeString(v["carrier_route_id"]), nativeInt32(v["android_subscription_id"]), nativeString(v["own_number_raw"]), nativeString(v["own_number_source"]), nativeInt64(v["route_observed_at_ms"])}
	return b, ok && nativeKinds(v, []string{"carrier_route_id", "own_number_raw", "own_number_source"}, nil, nil, []string{"android_subscription_id", "route_observed_at_ms"}) && nativeInt32Valid(v["android_subscription_id"]) && validOwnNumberSource(b.OwnNumberSource)
}

func decodeTarget(raw json.RawMessage) (nativeRcsTargetSnapshot, bool) {
	v, ok := nativeObject(raw, "proof_version", "peppy_conversation_id", "google_conversation_id", "conversation_type", "status", "send_mode", "read_only", "default_outgoing", "participants", "class", "canonical_recipient", "canonical_own_identity", "identity_region", "canonicalizer_revision", "selected_sim_id", "carrier_binding", "outgoing", "evidence")
	participants, participantsOK := decodeParticipants(v["participants"])
	binding, bindingOK := decodeCarrierBinding(v["carrier_binding"])
	outgoing, outgoingOK := decodeOutgoing(v["outgoing"])
	evidence, evidenceOK := decodeEvidence(v["evidence"])
	t := nativeRcsTargetSnapshot{nativeUint32(v["proof_version"]), nativeString(v["peppy_conversation_id"]), nativeString(v["google_conversation_id"]), nativeString(v["conversation_type"]), nativeString(v["status"]), nativeString(v["send_mode"]), nativeBool(v["read_only"]), nativeBool(v["default_outgoing"]), participants, nativeString(v["class"]), nativeString(v["canonical_recipient"]), nativeString(v["canonical_own_identity"]), nativeString(v["identity_region"]), nativeUint32(v["canonicalizer_revision"]), nativeString(v["selected_sim_id"]), binding, outgoing, evidence}
	return t, ok && participantsOK && bindingOK && outgoingOK && evidenceOK && nativeKinds(v, []string{"peppy_conversation_id", "google_conversation_id", "conversation_type", "status", "send_mode", "class", "canonical_recipient", "canonical_own_identity", "identity_region", "selected_sim_id"}, []string{"read_only", "default_outgoing"}, []string{"proof_version", "canonicalizer_revision"}, nil) && nativeUint32Valid(v["proof_version"]) && nativeUint32Valid(v["canonicalizer_revision"]) && validEnum(t.ConversationType, "Rcs", "Other") && validEnum(t.SendMode, "Automatic", "Other") && validEnum(t.Class, "OneToOne", "SelfChat", "Group")
}

func decodeParticipants(raw json.RawMessage) ([]nativeRcsParticipant, bool) {
	items, ok := nativeArray(raw)
	participants := make([]nativeRcsParticipant, len(items))
	for i, item := range items {
		v, valid := nativeObject(item, "identity", "google_participant_id", "identifier_type", "is_visible", "is_me")
		participants[i] = nativeRcsParticipant{nativeString(v["identity"]), nativeString(v["google_participant_id"]), nativeInt32(v["identifier_type"]), nativeBool(v["is_visible"]), nativeBool(v["is_me"])}
		ok = ok && valid && nativeKinds(v, []string{"identity", "google_participant_id"}, []string{"is_visible", "is_me"}, nil, []string{"identifier_type"}) && nativeInt32Valid(v["identifier_type"])
	}
	return participants, ok
}

func decodeCarrierBinding(raw json.RawMessage) (nativeRcsCarrierBinding, bool) {
	v, ok := nativeObject(raw, "carrier_route_id", "android_subscription_id", "is_current_default_sms", "own_number_raw", "own_number_source", "route_observed_at_ms", "route_context_revision", "same_handset_evidence_id")
	b := nativeRcsCarrierBinding{nativeString(v["carrier_route_id"]), nativeInt32(v["android_subscription_id"]), nativeBool(v["is_current_default_sms"]), nativeString(v["own_number_raw"]), nativeString(v["own_number_source"]), nativeInt64(v["route_observed_at_ms"]), nativeUint64(v["route_context_revision"]), nativeString(v["same_handset_evidence_id"])}
	return b, ok && nativeKinds(v, []string{"carrier_route_id", "own_number_raw", "own_number_source", "same_handset_evidence_id"}, []string{"is_current_default_sms"}, []string{"route_context_revision"}, []string{"android_subscription_id", "route_observed_at_ms"}) && nativeInt32Valid(v["android_subscription_id"]) && validOwnNumberSource(b.OwnNumberSource)
}

func decodeOutgoing(raw json.RawMessage) (nativeRcsOutgoing, bool) {
	v, ok := nativeObject(raw, "outgoing_id", "sim_participant_id", "own_identity_raw", "sim_payload", "rcs_chats_enabled")
	payload, payloadOK := decodeSIMPayload(v["sim_payload"])
	return nativeRcsOutgoing{nativeString(v["outgoing_id"]), nativeString(v["sim_participant_id"]), nativeString(v["own_identity_raw"]), payload, nativeBool(v["rcs_chats_enabled"])}, ok && payloadOK && nativeKinds(v, []string{"outgoing_id", "sim_participant_id", "own_identity_raw"}, []string{"rcs_chats_enabled"}, nil, nil)
}

func decodeSIMPayload(raw json.RawMessage) (nativeRcsSIMPayload, bool) {
	v, ok := nativeObject(raw, "two", "sim_number")
	return nativeRcsSIMPayload{nativeInt32(v["two"]), nativeInt32(v["sim_number"])}, ok && nativeKinds(v, nil, nil, nil, []string{"two", "sim_number"}) && nativeInt32Valid(v["two"]) && nativeInt32Valid(v["sim_number"])
}

func decodeEvidence(raw json.RawMessage) (nativeRcsEvidence, bool) {
	v, ok := nativeObject(raw, "lifetime", "lookup_id", "lookup_complete", "google_candidate_count", "settings_revision", "settings_provenance", "settings_observed_at_ms", "settings_stream_epoch", "settings_currentness", "current_settings_sims", "settings_validated_at_ms", "conversation_observed_at_ms")
	lifetime, lifetimeOK := decodeLifetime(v["lifetime"])
	sims, simsOK := decodeOutgoings(v["current_settings_sims"])
	e := nativeRcsEvidence{lifetime, nativeString(v["lookup_id"]), nativeBool(v["lookup_complete"]), nativeUint32(v["google_candidate_count"]), nativeUint64(v["settings_revision"]), nativeString(v["settings_provenance"]), nativeInt64(v["settings_observed_at_ms"]), nativeUint64(v["settings_stream_epoch"]), nativeString(v["settings_currentness"]), sims, nativeInt64(v["settings_validated_at_ms"]), nativeInt64(v["conversation_observed_at_ms"])}
	return e, ok && lifetimeOK && simsOK && nativeKinds(v, []string{"lookup_id", "settings_provenance", "settings_currentness"}, []string{"lookup_complete"}, []string{"google_candidate_count", "settings_revision", "settings_stream_epoch"}, []string{"settings_observed_at_ms", "settings_validated_at_ms", "conversation_observed_at_ms"}) && nativeUint32Valid(v["google_candidate_count"]) && validEnum(e.SettingsProvenance, "CurrentEvent", "OldEvent", "Unknown") && validEnum(e.SettingsCurrentness, "ContinuousVerifiedFeed", "Unproven")
}

func decodeLifetime(raw json.RawMessage) (nativeRcsLifetime, bool) {
	v, ok := nativeObject(raw, "runtime_id", "run_generation", "link_generation", "session_generation", "context_revision")
	return nativeRcsLifetime{nativeString(v["runtime_id"]), nativeUint64(v["run_generation"]), nativeUint64(v["link_generation"]), nativeUint64(v["session_generation"]), nativeUint64(v["context_revision"])}, ok && nativeKinds(v, []string{"runtime_id"}, nil, []string{"run_generation", "link_generation", "session_generation", "context_revision"}, nil)
}

func decodeOutgoings(raw json.RawMessage) ([]nativeRcsOutgoing, bool) {
	items, ok := nativeArray(raw)
	if !ok {
		return nil, false
	}
	result := make([]nativeRcsOutgoing, len(items))
	for i, item := range items {
		result[i], ok = decodeOutgoing(item)
		if !ok {
			return nil, false
		}
	}
	return result, true
}

func decodeDigest(raw json.RawMessage) ([32]byte, bool) {
	items, ok := nativeArray(raw)
	if !ok || len(items) != 32 {
		return [32]byte{}, false
	}
	var digest [32]byte
	for i, item := range items {
		n, valid := nativeUint64Checked(item)
		if !valid || n > math.MaxUint8 {
			return [32]byte{}, false
		}
		digest[i] = byte(n)
	}
	return digest, true
}

func nativeObject(raw []byte, fields ...string) (map[string]json.RawMessage, bool) {
	if !utf8.Valid(raw) {
		return nil, false
	}
	wanted := make(map[string]struct{}, len(fields))
	for _, field := range fields {
		wanted[field] = struct{}{}
	}
	decoder := json.NewDecoder(bytes.NewReader(raw))
	decoder.UseNumber()
	start, err := decoder.Token()
	if err != nil || start != json.Delim('{') {
		return nil, false
	}
	values := make(map[string]json.RawMessage, len(fields))
	for decoder.More() {
		token, err := decoder.Token()
		key, isString := token.(string)
		if err != nil || !isString {
			return nil, false
		}
		if _, wanted := wanted[key]; !wanted {
			return nil, false
		}
		if _, duplicate := values[key]; duplicate {
			return nil, false
		}
		var value json.RawMessage
		if decoder.Decode(&value) != nil || bytes.Equal(bytes.TrimSpace(value), []byte("null")) {
			return nil, false
		}
		values[key] = value
	}
	end, err := decoder.Token()
	if err != nil || end != json.Delim('}') || len(values) != len(wanted) {
		return nil, false
	}
	if _, err := decoder.Token(); err != io.EOF {
		return nil, false
	}
	return values, true
}

func nativeArray(raw []byte) ([]json.RawMessage, bool) {
	var items []json.RawMessage
	decoder := json.NewDecoder(bytes.NewReader(raw))
	decoder.UseNumber()
	if decoder.Decode(&items) != nil || items == nil {
		return nil, false
	}
	if _, err := decoder.Token(); err != io.EOF {
		return nil, false
	}
	for _, item := range items {
		if bytes.Equal(bytes.TrimSpace(item), []byte("null")) {
			return nil, false
		}
	}
	return items, true
}

func nativeKinds(values map[string]json.RawMessage, strings []string, bools []string, uints []string, ints []string) bool {
	for _, field := range strings {
		if _, ok := nativeStringChecked(values[field]); !ok {
			return false
		}
	}
	for _, field := range bools {
		if !bytes.Equal(values[field], []byte("true")) && !bytes.Equal(values[field], []byte("false")) {
			return false
		}
	}
	for _, field := range uints {
		if _, ok := nativeUint64Checked(values[field]); !ok {
			return false
		}
	}
	for _, field := range ints {
		if _, ok := nativeInt64Checked(values[field]); !ok {
			return false
		}
	}
	return true
}

func nativeString(raw json.RawMessage) string { value, _ := nativeStringChecked(raw); return value }
func nativeStringChecked(raw json.RawMessage) (string, bool) {
	if !validJSONStringUnicode(raw) {
		return "", false
	}
	var value string
	err := json.Unmarshal(raw, &value)
	return value, err == nil
}

func validJSONStringUnicode(raw json.RawMessage) bool {
	if len(raw) < 2 || raw[0] != '"' || raw[len(raw)-1] != '"' {
		return false
	}
	for i := 1; i < len(raw)-1; i++ {
		if raw[i] != '\\' {
			continue
		}
		i++
		if i >= len(raw)-1 || raw[i] != 'u' {
			continue
		}
		if i+4 >= len(raw) {
			return false
		}
		unit, ok := jsonUTF16Unit(raw[i+1 : i+5])
		if !ok {
			return false
		}
		i += 4
		if unit >= 0xD800 && unit <= 0xDBFF {
			if i+6 >= len(raw) || raw[i+1] != '\\' || raw[i+2] != 'u' {
				return false
			}
			low, ok := jsonUTF16Unit(raw[i+3 : i+7])
			if !ok || low < 0xDC00 || low > 0xDFFF {
				return false
			}
			i += 6
		} else if unit >= 0xDC00 && unit <= 0xDFFF {
			return false
		}
	}
	return true
}

func jsonUTF16Unit(raw []byte) (uint16, bool) {
	var value uint16
	for _, digit := range raw {
		value <<= 4
		switch {
		case digit >= '0' && digit <= '9':
			value |= uint16(digit - '0')
		case digit >= 'a' && digit <= 'f':
			value |= uint16(digit-'a') + 10
		case digit >= 'A' && digit <= 'F':
			value |= uint16(digit-'A') + 10
		default:
			return 0, false
		}
	}
	return value, true
}
func nativeBool(raw json.RawMessage) bool {
	var value bool
	if json.Unmarshal(raw, &value) != nil {
		return false
	}
	return value
}
func nativeUint64(raw json.RawMessage) uint64 { value, _ := nativeUint64Checked(raw); return value }
func nativeUint32(raw json.RawMessage) uint32 {
	value, ok := nativeUint64Checked(raw)
	if !ok || value > math.MaxUint32 {
		return 0
	}
	return uint32(value)
}
func nativeInt64(raw json.RawMessage) int64 { value, _ := nativeInt64Checked(raw); return value }
func nativeInt32(raw json.RawMessage) int32 {
	value, ok := nativeInt64Checked(raw)
	if !ok || value < math.MinInt32 || value > math.MaxInt32 {
		return 0
	}
	return int32(value)
}
func nativeInt32Valid(raw json.RawMessage) bool {
	value, ok := nativeInt64Checked(raw)
	return ok && value >= math.MinInt32 && value <= math.MaxInt32
}
func nativeUint32Valid(raw json.RawMessage) bool {
	value, ok := nativeUint64Checked(raw)
	return ok && value <= math.MaxUint32
}

func nativeUint64Checked(raw json.RawMessage) (uint64, bool) {
	s := string(raw)
	if !nativeInteger(s, false) {
		return 0, false
	}
	value, err := strconv.ParseUint(s, 10, 64)
	return value, err == nil
}
func nativeInt64Checked(raw json.RawMessage) (int64, bool) {
	s := string(raw)
	if !nativeInteger(s, true) {
		return 0, false
	}
	value, err := strconv.ParseInt(s, 10, 64)
	return value, err == nil
}
func nativeInteger(s string, signed bool) bool {
	if s == "" {
		return false
	}
	if signed && s[0] == '-' {
		s = s[1:]
		if s == "" {
			return false
		}
	}
	if len(s) > 1 && s[0] == '0' {
		return false
	}
	for _, r := range s {
		if r < '0' || r > '9' {
			return false
		}
	}
	return true
}
func validNativeTmpID(value string) bool { return len(value) >= 1 && len(value) <= 256 }
func validNativeBody(value string) bool {
	return len(value) >= 1 && len(value) <= 4096 && utf8.ValidString(value)
}
func validOwnNumberSource(value string) bool {
	return validEnum(value, "SubscriptionManagerPhoneNumber", "TelephonyLine1Number")
}
func validEnum(value string, allowed ...string) bool {
	return contains(allowed, value)
}
func contains(values []string, wanted string) bool {
	for _, value := range values {
		if value == wanted {
			return true
		}
	}
	return false
}
