"""Supplementary wiring guards; native focus journeys remain required."""
from pathlib import Path
import unittest


SOURCE = (Path(__file__).resolve().parents[2] / "clients/apple/Sources/RemoteNavigationViews.swift").read_text()


class AppleTVModalWiringTests(unittest.TestCase):
    def test_native_host_owns_back_and_presentation_fence(self):
        host = SOURCE.split("private struct RemoteChoicePresentation", 1)[1].split("extension View", 1)[0]
        tv, phone = host.split("#if os(tvOS)", 1)[1].split("#else", 1)
        self.assertIn(".fullScreenCover(", tv)
        self.assertIn("RemoteOwnedPresentationProbe(", tv)
        self.assertIn("RemoteChoiceExitAdapter(", tv)
        self.assertIn("modalPresentationDismissed(", tv)
        self.assertNotIn("fullScreenCover", phone)
        self.assertIn("content.overlay", phone)

    def test_choice_content_uses_selected_native_default_without_duplicate_back(self):
        panel = SOURCE.split("struct RemoteChoicePanel", 1)[1].split("struct RemoteOwnedPresentationProbe", 1)[0]
        self.assertIn(".remoteChoiceDefaultFocus(choice.selected", panel)
        self.assertIn(".remoteChoiceFocusScope(focusNamespace)", panel)
        self.assertIn("closeModal(returnFocus: choice.returnFocus)", panel)
        self.assertNotIn("RemoteChoiceExitAdapter", panel)
