package operations

import (
	"encoding/json"
	"errors"
	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"os"
	"testing"
)

func TestBrowserMutationConformance(t *testing.T) {
	bytes, err := os.ReadFile("../../../../packages/ui-bridge/mutation-cases.json")
	if err != nil {
		t.Fatal(err)
	}
	var cases []struct {
		Unknown bool
		Status  int
		Code    string
		Outcome string
	}
	if err := json.Unmarshal(bytes, &cases); err != nil {
		t.Fatal(err)
	}
	for _, c := range cases {
		err := settle("test", "abcdefghijklmnop", "", c.Unknown, &api.Problem{Status: c.Status, Code: c.Code})
		var unknown *UncertainError
		got := "settled"
		if errors.As(err, &unknown) {
			got = "unknown"
		}
		if got != c.Outcome {
			t.Errorf("%+v: got %s", c, got)
		}
	}
}
