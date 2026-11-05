package gmlink

import (
	"encoding/json"
	"errors"
)

const stateVersion = 1

const (
	stateUnlinked    = "unlinked"
	statePairing     = "pairing"
	stateLinked      = "linked"
	stateNeedsRelink = "needs_relink"
	stateSignedOut   = "signed_out"
)

type persistedState struct {
	Version     int             `json:"version"`
	Seq         uint64          `json:"seq"`
	State       string          `json:"state"`
	PairingFrom string          `json:"pairing_from,omitempty"`
	StopCode    string          `json:"stop_code"`
	Auth        json.RawMessage `json:"auth,omitempty"`
}

func decodeState(data []byte) (persistedState, error) {
	if len(data) == 0 {
		return persistedState{Version: stateVersion, State: stateUnlinked}, nil
	}
	var state persistedState
	if err := json.Unmarshal(data, &state); err != nil || state.Version != stateVersion || !validState(state.State) {
		return persistedState{}, errors.New("invalid_state")
	}
	if state.State == statePairing {
		if state.PairingFrom == "" {
			// Pairing envelopes written before pairing_from existed can only
			// conservatively reopen as unlinked.
			state.PairingFrom = stateUnlinked
		}
		if !validPairingOrigin(state.PairingFrom) {
			return persistedState{}, errors.New("invalid_state")
		}
		state.State, state.PairingFrom = state.PairingFrom, ""
	}
	return state, nil
}

func validPairingOrigin(state string) bool {
	switch state {
	case stateUnlinked, stateNeedsRelink, stateSignedOut:
		return true
	default:
		return false
	}
}

func validState(state string) bool {
	switch state {
	case stateUnlinked, statePairing, stateLinked, stateNeedsRelink, stateSignedOut:
		return true
	default:
		return false
	}
}
