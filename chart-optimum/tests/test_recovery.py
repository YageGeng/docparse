"""Check the tolerant table recovery that the release's malformed output requires."""

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from runtime import (
    RELIABLE_DISTANCE,
    collect_numbers,
    parse_chart,
    recover_table,
    reliability,
)

# The release writes `{` where a title string belongs, so the document never parses.
DROPPED_BRACE = """{
    "title": {
    "source": "None",
    "x_title": "Temperature",
    "y_title": "Count of Readings",
    "values": {
        "25": {
            "15-14": "1822",
            "14-15": "2681",
            "22-23": "61775"
        }
    }
}"""

# The release drops a quote in the middle of one row, which also breaks the whole document.
DROPPED_QUOTE = """{
    "title": "None",
    "source": "None",
    "x_title": "Time of Day ( UTC",
    "y_title": "Total Power (WTO)",
    "values": {
        "Total Power (W)": {
            "00:00": "679",
            "02:00": "657",
            "22: "720",
            "00:00": "665"
        },
        "Ambient Temp": {
            "00:00": "602"
        }
    }
}"""

# The hardest chart degenerates before it ever opens `values`.
DEGENERATE = """{
    "title": "Comparly Acc vs Biscuit's Biscuit's Biscuit' Biscuit'",
    "source": " "'Don't fa
    "values": "Biscuit' Biscuit' Biscuit"
}"""

VALID = """{
    "title": "None",
    "values": {"Total Power (W)": {"00:00": "679", "02:00": "657"}}
}"""


class RecoveryTests(unittest.TestCase):
    """Keep extracted rows available even when the release does not close valid JSON."""

    def test_recovers_rows_when_a_brace_replaces_a_value(self):
        """Parse the histogram rows out of a document whose title value is a brace."""
        table = recover_table(DROPPED_BRACE)
        self.assertEqual(list(table), ["25"])
        self.assertEqual(
            table["25"],
            [
                {"label": "15-14", "value": "1822"},
                {"label": "14-15", "value": "2681"},
                {"label": "22-23", "value": "61775"},
            ],
        )

    def test_recovers_rows_around_a_dropped_quote(self):
        """Keep every intact row, attribute it to its series, and never invent the broken one."""
        table = recover_table(DROPPED_QUOTE)
        self.assertEqual(list(table), ["Total Power (W)", "Ambient Temp"])
        self.assertEqual(
            [row["label"] for row in table["Total Power (W)"]],
            ["00:00", "02:00", "00:00"],
        )
        self.assertEqual(table["Ambient Temp"], [{"label": "00:00", "value": "602"}])

    def test_returns_nothing_without_a_values_section(self):
        """Report an empty table for output that never reached the chart schema."""
        self.assertEqual(recover_table(DEGENERATE), {})
        self.assertEqual(recover_table(""), {})

    def test_keeps_grouped_values_whole(self):
        """Keep a thousands separator from truncating a value to its first digit."""
        text = (
            '{"values": {"Revenue (USD)": {'
            '"2019": "1,234,567", "2020": "89,012", "2021": "7.5"}}}'
        )
        table = recover_table(text)
        self.assertEqual(
            table["Revenue (USD)"],
            [
                {"label": "2019", "value": "1234567"},
                {"label": "2020", "value": "89012"},
                {"label": "2021", "value": "7.5"},
            ],
        )

    def test_strict_parse_is_unchanged(self):
        """Keep `data` a strict parse so a null still means the release did not close JSON."""
        self.assertIsNone(parse_chart(DROPPED_BRACE))
        self.assertIsNone(parse_chart(DROPPED_QUOTE))
        parsed = parse_chart(VALID)
        assert isinstance(parsed, dict)
        self.assertEqual(parsed["values"]["Total Power (W)"]["00:00"], "679")

    def test_never_reads_past_the_values_object(self):
        """Keep fields that follow `values` from becoming fabricated data rows."""
        # A numeric `source` after an empty `values` object must contribute nothing at all.
        self.assertEqual(recover_table('{"title":"x","values":{},"source":"2020"}'), {})
        # A numeric field after a populated `values` object must not join the series either.
        table = recover_table('{"values":{"S":{"a":"1"}},"x_title":"2020"}')
        self.assertEqual(table, {"S": [{"label": "a", "value": "1"}]})
        # Reserve the schema's own keys even when they appear inside the object.
        self.assertEqual(recover_table('{"values":{"source":2020}}'), {})

    def test_recovers_rows_when_the_release_omits_values(self):
        """Keep rows written straight after `title`, without a `values` wrapper."""
        table = recover_table(
            '{"title": "Quarterly Revenue", "source": "None", '
            '"North": "120", "South": "200", "East": "150"}'
        )
        self.assertEqual(
            table,
            {
                "": [
                    {"label": "North", "value": "120"},
                    {"label": "South", "value": "200"},
                    {"label": "East", "value": "150"},
                ]
            },
        )

    def test_missing_closing_brace_still_yields_rows(self):
        """Keep the reading when the release drops the brace that ends `values`."""
        table = recover_table('{"values": {"S": {"a": "1", "b": "2"')
        self.assertEqual(
            table["S"],
            [{"label": "a", "value": "1"}, {"label": "b", "value": "2"}],
        )

    def test_values_that_are_not_an_object_are_rejected(self):
        """Reject a non-object `values` instead of raising out of the inference call."""
        self.assertEqual(collect_numbers("not a mapping"), [])
        self.assertEqual(collect_numbers([1, 2, 3]), [])
        self.assertEqual(collect_numbers({"Total": {"00:00": "679"}}), [679.0])
        distance, reliable = reliability('{"values": "broken"}', [0.5])
        self.assertIsNone(distance)
        self.assertFalse(reliable)

    def test_reliability_matches_the_release_reading(self):
        """Normalize parsed values and report the release's L1 verdict."""
        text = '{"values": {"S": {"a": "0", "b": "10"}}}'
        # Normalized to [0, 1]; a head reading the same shape is accepted, a flat one is not.
        distance, reliable = reliability(text, [0.0, 1.0])
        self.assertEqual(distance, 0.0)
        self.assertTrue(reliable)
        distance, reliable = reliability(text, [0.9, 0.9])
        self.assertIsNotNone(distance)
        assert distance is not None
        self.assertGreater(distance, RELIABLE_DISTANCE)
        self.assertFalse(reliable)


if __name__ == "__main__":
    unittest.main()
