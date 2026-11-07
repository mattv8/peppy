package gmlink

import (
	"bytes"
	"encoding/json"
	"fmt"
	"sync"
	"testing"
	"time"

	"github.com/google/uuid"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

const nativeCommitMatrixWait = 2 * time.Second

func awaitNativeCommitAction(t *testing.T, phone *phoneFixture, want gmproto.ActionType) {
	t.Helper()
	timer := time.NewTimer(nativeCommitMatrixWait)
	defer timer.Stop()
	for {
		select {
		case got := <-phone.actionReached:
			if got == want {
				return
			}
		case <-timer.C:
			t.Fatalf("did not reach %s", want)
		}
	}
}

func awaitNativeCommitResult(t *testing.T, done <-chan string) string {
	t.Helper()
	select {
	case result := <-done:
		return result
	case <-time.After(nativeCommitMatrixWait):
		t.Fatal("commit did not finish")
		return ""
	}
}

func TestNativeSendCommitRevocationAdmissionFences(t *testing.T) {
	t.Run("revocation before admission rejects without send", func(t *testing.T) {
		link, phone, cleanup, ticket, commit := nativeDispatchFixture(t)
		defer cleanup()
		reached := make(chan struct{})
		release := make(chan struct{})
		var releaseOnce sync.Once
		unblock := func() { releaseOnce.Do(func() { close(release) }) }
		defer unblock()
		link.mu.Lock()
		link.beforeSendAdmission = func() {
			close(reached)
			<-release
		}
		link.mu.Unlock()
		done := make(chan string, 1)
		go func() { done <- committer(t, link).CommitNativeRcs(ticket, nativeCommitJSON(t, commit)) }()
		select {
		case <-reached:
		case <-time.After(nativeCommitMatrixWait):
			t.Fatal("commit did not reach admission fence")
		}
		if got := stage2Code(t, link.RevokeNativeSend(ticket)); got != "revoked" {
			t.Fatalf("revoke = %s", got)
		}
		unblock()
		if got := stage2Code(t, awaitNativeCommitResult(t, done)); got != "rejected" {
			t.Fatalf("commit = %s", got)
		}
		if phone.countAction("SEND_MESSAGE") != 0 {
			t.Fatal("revoked commit sent a message")
		}
	})

	t.Run("revocation after admission is late and preserves acceptance", func(t *testing.T) {
		link, phone, cleanup, ticket, commit := nativeDispatchFixture(t)
		defer cleanup()
		defer phone.openGate()
		phone.configure(func(knobs *fixtureKnobs) {
			knobs.gateSend = true
			knobs.gateAction = gmproto.ActionType_SEND_MESSAGE
		})
		done := make(chan string, 1)
		go func() { done <- committer(t, link).CommitNativeRcs(ticket, nativeCommitJSON(t, commit)) }()
		awaitNativeCommitAction(t, phone, gmproto.ActionType_SEND_MESSAGE)
		if got := stage2Code(t, link.RevokeNativeSend(ticket)); got != "late" {
			t.Fatalf("revoke = %s", got)
		}
		phone.openGate()
		if got := stage2Code(t, awaitNativeCommitResult(t, done)); got != "accepted" {
			t.Fatalf("commit = %s", got)
		}
		if phone.countAction("SEND_MESSAGE") != 1 {
			t.Fatal("admitted commit did not send exactly once")
		}
	})
}

func TestNativeSendCommitConsumesTicketOnceUnderConcurrency(t *testing.T) {
	link, phone, cleanup, ticket, commit := nativeDispatchFixture(t)
	defer cleanup()
	encoded := nativeCommitJSON(t, commit)
	start := make(chan struct{})
	results := make(chan string, 2)
	var group sync.WaitGroup
	for range 2 {
		group.Add(1)
		go func() {
			defer group.Done()
			<-start
			results <- stage2Code(t, committer(t, link).CommitNativeRcs(ticket, encoded))
		}()
	}
	close(start)
	group.Wait()
	close(results)
	accepted, rejected := 0, 0
	for result := range results {
		switch result {
		case "accepted":
			accepted++
		case "rejected":
			rejected++
		default:
			t.Fatalf("commit = %s", result)
		}
	}
	if accepted != 1 || rejected != 1 {
		t.Fatalf("accepted=%d rejected=%d", accepted, rejected)
	}
	if phone.countAction("SEND_MESSAGE") != 1 || phone.countAction("GET_CONVERSATION") != 2 {
		t.Fatal("concurrent commits did not perform one fresh read and one send")
	}

	now := time.Now().UnixMilli()
	request, err := json.Marshal(map[string]any{
		"command_id": uuid.NewString(), "canonicalizer_revision": 1, "identity_region": "US",
		"carrier_binding": map[string]any{"carrier_route_id": "android-subscription-1", "android_subscription_id": 1, "own_number_raw": "+12025550101", "own_number_source": "SubscriptionManagerPhoneNumber", "route_observed_at_ms": now},
	})
	if err != nil {
		t.Fatal(err)
	}
	var successor nativeRcsPrepareResult
	if err := json.Unmarshal([]byte(link.PrepareNativeRcs(string(request))), &successor); err != nil || successor.Code != "prepared" {
		t.Fatalf("successor prepare = %q, %v", successor.Code, err)
	}
	if got := stage2Code(t, link.RevokeNativeSend(ticket)); got != "none" {
		t.Fatalf("old revoke = %s", got)
	}
	link.mu.Lock()
	remaining := link.nativeTicket
	link.mu.Unlock()
	if remaining == nil || remaining.id != successor.TicketID {
		t.Fatal("old revoke cleared successor ticket")
	}
}

func TestNativeSendCommitProjectionMutationMatrix(t *testing.T) {
	_, _, cleanup, _, commit := nativeDispatchFixture(t)
	cleanup()
	valid := nativeCommitJSON(t, commit)
	var normalized any
	decoder := json.NewDecoder(bytes.NewReader([]byte(valid)))
	decoder.UseNumber()
	if err := decoder.Decode(&normalized); err != nil {
		t.Fatal(err)
	}
	for _, path := range nativeCommitLeafPaths(normalized, nil) {
		path := append([]string(nil), path...)
		if path[0] == "message_body" || path[0] == "tmp_id" {
			continue
		}
		t.Run(nativeCommitPathName(path), func(t *testing.T) {
			link, phone, caseCleanup, ticket, caseCommit := nativeDispatchFixture(t)
			defer caseCleanup()
			caseJSON := nativeCommitJSON(t, caseCommit)
			var value any
			caseDecoder := json.NewDecoder(bytes.NewReader([]byte(caseJSON)))
			caseDecoder.UseNumber()
			if err := caseDecoder.Decode(&value); err != nil {
				t.Fatal(err)
			}
			nativeCommitMutateLeaf(t, value, path)
			encoded, err := json.Marshal(value)
			if err != nil {
				t.Fatal(err)
			}
			if got := stage2Code(t, committer(t, link).CommitNativeRcs(ticket, string(encoded))); got != "rejected" {
				t.Fatalf("mutation at %s = %s", nativeCommitPathName(path), got)
			}
			if phone.countAction("GET_CONVERSATION") != 1 || phone.countAction("SEND_MESSAGE") != 0 {
				t.Fatalf("mutation at %s reached commit transport", nativeCommitPathName(path))
			}
		})
	}

	t.Run("body and tmp id remain captured native values", func(t *testing.T) {
		link, phone, cleanup, ticket, commit := nativeDispatchFixture(t)
		defer cleanup()
		commit["message_body"] = "different but valid core-issued body"
		commit["tmp_id"] = "different-valid-native-tmp"
		if got := stage2Code(t, committer(t, link).CommitNativeRcs(ticket, nativeCommitJSON(t, commit))); got != "accepted" {
			t.Fatalf("commit = %s", got)
		}
		phone.mu.Lock()
		requests := append([]*gmproto.SendMessageRequest(nil), phone.sendRequests...)
		phone.mu.Unlock()
		if len(requests) != 1 || requests[0].GetTmpID() != "different-valid-native-tmp" || requests[0].GetMessagePayload().GetMessageInfo()[0].GetMessageContent().GetContent() != "different but valid core-issued body" {
			t.Fatal("send payload did not preserve captured body and tmp id")
		}
	})
}

func nativeCommitLeafPaths(value any, prefix []string) [][]string {
	switch value := value.(type) {
	case map[string]any:
		var paths [][]string
		for key, child := range value {
			paths = append(paths, nativeCommitLeafPaths(child, append(prefix, key))...)
		}
		return paths
	case []any:
		var paths [][]string
		for index, child := range value {
			paths = append(paths, nativeCommitLeafPaths(child, append(prefix, fmt.Sprintf("%d", index)))...)
		}
		return paths
	default:
		return [][]string{append([]string(nil), prefix...)}
	}
}

func TestNativeCommitMutationPathsDoNotAliasSiblings(t *testing.T) {
	prefix := make([]string, 1, 8)
	prefix[0] = "target"
	paths := nativeCommitLeafPaths(map[string]any{
		"lifetime":     map[string]any{"runtime_id": "run", "run_generation": json.Number("1")},
		"participants": []any{map[string]any{"identity": "peer", "is_me": false}},
	}, prefix)
	want := map[string]bool{
		"[target lifetime runtime_id]":     true,
		"[target lifetime run_generation]": true,
		"[target participants 0 identity]": true,
		"[target participants 0 is_me]":    true,
	}
	for _, path := range paths {
		name := nativeCommitPathName(path)
		if !want[name] {
			t.Fatalf("duplicate or corrupted mutation path: %s", name)
		}
		delete(want, name)
	}
	if len(want) != 0 {
		t.Fatalf("missing mutation paths: %v", want)
	}
}

func nativeCommitMutateLeaf(t *testing.T, value any, path []string) {
	t.Helper()
	parent := value
	for _, part := range path[:len(path)-1] {
		switch current := parent.(type) {
		case map[string]any:
			parent = current[part]
		case []any:
			var index int
			if _, err := fmt.Sscanf(part, "%d", &index); err != nil {
				t.Fatal(err)
			}
			parent = current[index]
		}
	}
	last := path[len(path)-1]
	mutate := func(current any) any {
		switch current := current.(type) {
		case string:
			switch current {
			case "Rcs":
				return "Other"
			case "Automatic":
				return "Other"
			case "OneToOne":
				return "SelfChat"
			case "SubscriptionManagerPhoneNumber":
				return "TelephonyLine1Number"
			default:
				return current + "-changed"
			}
		case json.Number:
			if current.String() == "1" {
				return json.Number("2")
			}
			return json.Number("1")
		case bool:
			return !current
		default:
			t.Fatalf("unexpected leaf type %T", current)
			return nil
		}
	}
	switch current := parent.(type) {
	case map[string]any:
		current[last] = mutate(current[last])
	case []any:
		var index int
		if _, err := fmt.Sscanf(last, "%d", &index); err != nil {
			t.Fatal(err)
		}
		current[index] = mutate(current[index])
	default:
		t.Fatalf("invalid path parent %T", parent)
	}
}

func nativeCommitPathName(path []string) string { return fmt.Sprintf("%v", path) }
