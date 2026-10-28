"""Connectivity, fallback, and alternative-route advanced controls."""

from qgis.PyQt.QtWidgets import (
    QCheckBox,
    QComboBox,
    QFormLayout,
    QGroupBox,
    QLabel,
    QLineEdit,
    QMessageBox,
    QVBoxLayout,
    QWidget,
)

from .compat import message_box_button
from .constants import SETTINGS_PREFIX


class AdvancedControlsMixin:
    def _build_advanced_controls(self, prefix, include_failure_modes):
        container = QWidget()
        layout = QVBoxLayout(container)
        layout.setContentsMargins(0, 0, 0, 0)

        connectivity_group = QGroupBox("Disconnected-network handling")
        connectivity_form = QFormLayout(connectivity_group)
        connectivity_mode_combo = QComboBox()
        connectivity_mode_combo.addItem("Strict", "strict")
        connectivity_mode_combo.addItem("Ignore unreachable", "ignore_unreachable")
        connectivity_mode_combo.addItem(
            "Hop origin to reachable component",
            "hop_origin_to_nearest_reachable_component",
        )
        connectivity_mode_combo.addItem(
            "Hop destination to reachable component",
            "hop_destination_to_nearest_reachable_component",
        )
        connectivity_mode_combo.addItem("Hop either end", "hop_either_end")
        connectivity_mode_combo.setToolTip(
            "What to do when a point snaps to a network component the other "
            "point cannot reach. Strict fails the request; the hop policies "
            "bridge to the nearest reachable component."
        )
        max_hop_distance_edit = QLineEdit("")
        max_hop_distance_edit.setToolTip(
            "Optional cap in meters for component-hop bridging. Empty means unlimited."
        )
        report_hop_distance_check = QCheckBox("Report hop distance separately")
        connectivity_form.addRow("Policy", connectivity_mode_combo)
        connectivity_form.addRow("Max hop distance m", max_hop_distance_edit)
        connectivity_form.addRow("", report_hop_distance_check)
        layout.addWidget(connectivity_group)

        setattr(self, "{}_connectivity_mode_combo".format(prefix), connectivity_mode_combo)
        setattr(self, "{}_max_hop_distance_edit".format(prefix), max_hop_distance_edit)
        setattr(
            self,
            "{}_report_hop_distance_check".format(prefix),
            report_hop_distance_check,
        )

        if include_failure_modes:
            alternatives_group = QGroupBox("Alternative routes")
            alternatives_form = QFormLayout(alternatives_group)
            alternative_count_edit = QLineEdit("1")
            alternative_cost_ratio_edit = QLineEdit("1.35")
            alternative_min_jaccard_edit = QLineEdit("0.2")
            alternatives_form.addRow("Max routes", alternative_count_edit)
            alternatives_form.addRow("Max cost ratio", alternative_cost_ratio_edit)
            alternatives_form.addRow("Min edge-set distance", alternative_min_jaccard_edit)
            layout.addWidget(alternatives_group)
            setattr(
                self,
                "{}_alternative_count_edit".format(prefix),
                alternative_count_edit,
            )
            setattr(
                self,
                "{}_alternative_cost_ratio_edit".format(prefix),
                alternative_cost_ratio_edit,
            )
            setattr(
                self,
                "{}_alternative_min_jaccard_edit".format(prefix),
                alternative_min_jaccard_edit,
            )

            unsafe_note = QLabel(
                "Unsafe failure modes are off by default, never persist between "
                "QGIS sessions, and always ask for confirmation before a request "
                "is sent. Enable them only for explicit degraded-routing analysis."
            )
            unsafe_note.setWordWrap(True)
            layout.addWidget(unsafe_note)

            unsafe_widget = QWidget()
            unsafe_layout = QVBoxLayout(unsafe_widget)
            unsafe_layout.setContentsMargins(0, 0, 0, 0)

            failure_group = QGroupBox("Unsafe failure modes")
            failure_form = QFormLayout(failure_group)
            allow_reverse_oneway_check = QCheckBox("Allow reverse oneway traversal")
            allow_illegal_turn_check = QCheckBox("Allow illegal turns")
            ignore_turn_restrictions_check = QCheckBox("Ignore turn restrictions")
            allow_uturn_check = QCheckBox("Allow normally forbidden U-turns")
            auto_relax_unreachable_check = QCheckBox(
                "Auto resolve unreachable with least-permissive degraded route"
            )
            reverse_penalty_edit = QLineEdit("")
            illegal_turn_penalty_edit = QLineEdit("")
            ignored_restriction_penalty_edit = QLineEdit("")
            forbidden_uturn_penalty_edit = QLineEdit("")
            max_illegal_distance_edit = QLineEdit("")
            max_illegal_turns_edit = QLineEdit("")
            failure_form.addRow("", auto_relax_unreachable_check)
            failure_form.addRow("", allow_reverse_oneway_check)
            failure_form.addRow("", allow_illegal_turn_check)
            failure_form.addRow("", ignore_turn_restrictions_check)
            failure_form.addRow("", allow_uturn_check)
            failure_form.addRow("Reverse oneway penalty s", reverse_penalty_edit)
            failure_form.addRow("Illegal turn penalty s", illegal_turn_penalty_edit)
            failure_form.addRow(
                "Ignored restriction penalty s",
                ignored_restriction_penalty_edit,
            )
            failure_form.addRow("Forbidden U-turn penalty s", forbidden_uturn_penalty_edit)
            failure_form.addRow("Max illegal distance m", max_illegal_distance_edit)
            failure_form.addRow("Max illegal turns", max_illegal_turns_edit)
            unsafe_layout.addWidget(failure_group)
            unsafe_toggle = self._make_toggle_section(
                "Unsafe Failure Modes", unsafe_widget
            )
            layout.addWidget(unsafe_toggle)
            layout.addWidget(unsafe_widget)

            for check in [
                auto_relax_unreachable_check,
                allow_reverse_oneway_check,
                allow_illegal_turn_check,
                ignore_turn_restrictions_check,
                allow_uturn_check,
            ]:
                check.toggled.connect(
                    lambda _checked, p=prefix: self.update_unsafe_indicator(p)
                )

            setattr(self, "{}_unsafe_toggle".format(prefix), unsafe_toggle)
            setattr(
                self,
                "{}_allow_reverse_oneway_check".format(prefix),
                allow_reverse_oneway_check,
            )
            setattr(
                self,
                "{}_auto_relax_unreachable_check".format(prefix),
                auto_relax_unreachable_check,
            )
            setattr(
                self,
                "{}_allow_illegal_turn_check".format(prefix),
                allow_illegal_turn_check,
            )
            setattr(
                self,
                "{}_ignore_turn_restrictions_check".format(prefix),
                ignore_turn_restrictions_check,
            )
            setattr(self, "{}_allow_uturn_check".format(prefix), allow_uturn_check)
            setattr(
                self,
                "{}_reverse_penalty_edit".format(prefix),
                reverse_penalty_edit,
            )
            setattr(
                self,
                "{}_illegal_turn_penalty_edit".format(prefix),
                illegal_turn_penalty_edit,
            )
            setattr(
                self,
                "{}_ignored_restriction_penalty_edit".format(prefix),
                ignored_restriction_penalty_edit,
            )
            setattr(
                self,
                "{}_forbidden_uturn_penalty_edit".format(prefix),
                forbidden_uturn_penalty_edit,
            )
            setattr(
                self,
                "{}_max_illegal_distance_edit".format(prefix),
                max_illegal_distance_edit,
            )
            setattr(
                self,
                "{}_max_illegal_turns_edit".format(prefix),
                max_illegal_turns_edit,
            )

        return container

    def build_connectivity_policy(self, prefix):
        policy = {
            "disconnected": getattr(self, "{}_connectivity_mode_combo".format(prefix)).currentData()
            or "strict"
        }
        max_hop_distance = self.parse_optional_float(
            getattr(self, "{}_max_hop_distance_edit".format(prefix)).text(),
            "Max hop distance",
        )
        if max_hop_distance is not None:
            policy["max_hop_distance_m"] = max_hop_distance
        if getattr(self, "{}_report_hop_distance_check".format(prefix)).isChecked():
            policy["report_hop_distance_separately"] = True
        return policy

    def build_fallback_policy(self, prefix):
        allow_reverse_oneway = getattr(
            self, "{}_allow_reverse_oneway_check".format(prefix)
        ).isChecked()
        allow_illegal_turn = getattr(
            self, "{}_allow_illegal_turn_check".format(prefix)
        ).isChecked()
        ignore_turn_restrictions = getattr(
            self, "{}_ignore_turn_restrictions_check".format(prefix)
        ).isChecked()
        allow_uturn = getattr(self, "{}_allow_uturn_check".format(prefix)).isChecked()
        penalties = {}
        reverse_penalty = self.parse_optional_float(
            getattr(self, "{}_reverse_penalty_edit".format(prefix)).text(),
            "Reverse oneway penalty",
        )
        illegal_turn_penalty = self.parse_optional_float(
            getattr(self, "{}_illegal_turn_penalty_edit".format(prefix)).text(),
            "Illegal turn penalty",
        )
        ignored_restriction_penalty = self.parse_optional_float(
            getattr(self, "{}_ignored_restriction_penalty_edit".format(prefix)).text(),
            "Ignored restriction penalty",
        )
        forbidden_uturn_penalty = self.parse_optional_float(
            getattr(self, "{}_forbidden_uturn_penalty_edit".format(prefix)).text(),
            "Forbidden U-turn penalty",
        )
        if reverse_penalty is not None:
            penalties["reverse_oneway_penalty_s"] = reverse_penalty
        if illegal_turn_penalty is not None:
            penalties["illegal_turn_penalty_s"] = illegal_turn_penalty
        if ignored_restriction_penalty is not None:
            penalties["ignored_turn_restriction_penalty_s"] = ignored_restriction_penalty
        if forbidden_uturn_penalty is not None:
            penalties["forbidden_uturn_penalty_s"] = forbidden_uturn_penalty

        policy = {
            "allow_reverse_oneway": allow_reverse_oneway,
            "allow_illegal_turn": allow_illegal_turn,
            "ignore_turn_restrictions": ignore_turn_restrictions,
            "allow_uturn_where_normally_forbidden": allow_uturn,
            "auto_relax_unreachable": getattr(
                self, "{}_auto_relax_unreachable_check".format(prefix)
            ).isChecked(),
        }
        if penalties:
            policy["penalties"] = penalties

        max_illegal_distance = self.parse_optional_float(
            getattr(self, "{}_max_illegal_distance_edit".format(prefix)).text(),
            "Max illegal distance",
        )
        if max_illegal_distance is not None:
            policy["max_illegal_distance_m"] = max_illegal_distance

        max_illegal_turns = self.parse_optional_int(
            getattr(self, "{}_max_illegal_turns_edit".format(prefix)).text(),
            "Max illegal turns",
        )
        if max_illegal_turns is not None:
            policy["max_illegal_turns"] = max_illegal_turns

        return policy

    def build_alternative_options(self, prefix):
        max_routes = self.parse_optional_int(
            getattr(self, "{}_alternative_count_edit".format(prefix)).text(),
            "Alternative max routes",
        )
        if max_routes is None or max_routes <= 1:
            return {"max_routes": 1}
        options = {"max_routes": max_routes}
        max_cost_ratio = self.parse_optional_float(
            getattr(self, "{}_alternative_cost_ratio_edit".format(prefix)).text(),
            "Alternative max cost ratio",
        )
        if max_cost_ratio is not None:
            options["max_cost_ratio"] = max_cost_ratio
        min_jaccard = self.parse_optional_float(
            getattr(self, "{}_alternative_min_jaccard_edit".format(prefix)).text(),
            "Alternative min edge-set distance",
        )
        if min_jaccard is not None:
            options["min_jaccard_distance"] = min_jaccard
        return options

    def has_unsafe_failure_modes(self, prefix):
        policy = self.build_fallback_policy(prefix)
        return any(
            policy.get(flag)
            for flag in [
                "allow_reverse_oneway",
                "allow_illegal_turn",
                "ignore_turn_restrictions",
                "allow_uturn_where_normally_forbidden",
                "auto_relax_unreachable",
            ]
        )

    def confirm_unsafe_failure_modes(self, prefix, analysis_label):
        if not self.has_unsafe_failure_modes(prefix):
            return True

        policy = self.build_fallback_policy(prefix)
        enabled = []
        for key, label in [
            (
                "auto_relax_unreachable",
                "auto least-permissive degraded route recovery",
            ),
            ("allow_reverse_oneway", "reverse oneway"),
            ("allow_illegal_turn", "illegal turns"),
            ("ignore_turn_restrictions", "ignored turn restrictions"),
            ("allow_uturn_where_normally_forbidden", "forbidden U-turns"),
        ]:
            if policy.get(key):
                enabled.append(label)

        reply = QMessageBox.warning(
            self,
            "netweevil unsafe analysis",
            "This {} request enables unsafe degraded-routing modes: {}.\n\n"
            "These options are explicit fallback analysis only. Continue?".format(
                analysis_label, ", ".join(enabled)
            ),
            message_box_button("Yes") | message_box_button("No"),
            message_box_button("No"),
        )
        return reply == message_box_button("Yes")

    def update_unsafe_indicator(self, prefix):
        """Mark the collapsed section toggles red while unsafe modes are enabled."""
        check_names = [
            "{}_auto_relax_unreachable_check",
            "{}_allow_reverse_oneway_check",
            "{}_allow_illegal_turn_check",
            "{}_ignore_turn_restrictions_check",
            "{}_allow_uturn_check",
        ]
        active = any(
            getattr(self, name.format(prefix)).isChecked() for name in check_names
        )
        style = "color: #c92a2a; font-weight: bold;" if active else ""
        tooltip = (
            "Unsafe degraded-routing modes are enabled for this analysis."
            if active
            else ""
        )
        for toggle_name in ["{}_unsafe_toggle", "{}_advanced_toggle"]:
            toggle = getattr(self, toggle_name.format(prefix), None)
            if toggle is not None:
                toggle.setStyleSheet(style)
                toggle.setToolTip(tooltip)

    def save_advanced_settings(self, settings, prefix, include_failure_modes):
        values = {
            "{}_advanced_visible".format(prefix): getattr(
                self, "{}_advanced_toggle".format(prefix)
            ).isChecked(),
            "{}_connectivity_mode".format(prefix): getattr(
                self, "{}_connectivity_mode_combo".format(prefix)
            ).currentData(),
            "{}_max_hop_distance".format(prefix): getattr(
                self, "{}_max_hop_distance_edit".format(prefix)
            ).text().strip(),
            "{}_report_hop_distance".format(prefix): getattr(
                self, "{}_report_hop_distance_check".format(prefix)
            ).isChecked(),
        }
        if include_failure_modes:
            # Failure-mode checkboxes reset at the start of every QGIS session.
            values.update(
                {
                    "{}_unsafe_visible".format(prefix): getattr(
                        self, "{}_unsafe_toggle".format(prefix)
                    ).isChecked(),
                    "{}_reverse_penalty".format(prefix): getattr(
                        self, "{}_reverse_penalty_edit".format(prefix)
                    ).text().strip(),
                    "{}_illegal_turn_penalty".format(prefix): getattr(
                        self, "{}_illegal_turn_penalty_edit".format(prefix)
                    ).text().strip(),
                    "{}_ignored_restriction_penalty".format(prefix): getattr(
                        self, "{}_ignored_restriction_penalty_edit".format(prefix)
                    ).text().strip(),
                    "{}_forbidden_uturn_penalty".format(prefix): getattr(
                        self, "{}_forbidden_uturn_penalty_edit".format(prefix)
                    ).text().strip(),
                    "{}_max_illegal_distance".format(prefix): getattr(
                        self, "{}_max_illegal_distance_edit".format(prefix)
                    ).text().strip(),
                    "{}_max_illegal_turns".format(prefix): getattr(
                        self, "{}_max_illegal_turns_edit".format(prefix)
                    ).text().strip(),
                    "{}_alternative_count".format(prefix): getattr(
                        self, "{}_alternative_count_edit".format(prefix)
                    ).text().strip(),
                    "{}_alternative_cost_ratio".format(prefix): getattr(
                        self, "{}_alternative_cost_ratio_edit".format(prefix)
                    ).text().strip(),
                    "{}_alternative_min_jaccard".format(prefix): getattr(
                        self, "{}_alternative_min_jaccard_edit".format(prefix)
                    ).text().strip(),
                }
            )
        for key, value in values.items():
            settings.setValue("{}/{}".format(SETTINGS_PREFIX, key), value)

    def load_advanced_settings(self, prefix, include_failure_modes):
        self.set_combo_by_data(
            getattr(self, "{}_connectivity_mode_combo".format(prefix)),
            self.read_setting("{}_connectivity_mode".format(prefix), "strict"),
        )
        getattr(self, "{}_max_hop_distance_edit".format(prefix)).setText(
            self.read_setting("{}_max_hop_distance".format(prefix), "")
        )
        getattr(self, "{}_report_hop_distance_check".format(prefix)).setChecked(
            self.read_bool_setting("{}_report_hop_distance".format(prefix), False)
        )
        getattr(self, "{}_advanced_toggle".format(prefix)).setChecked(
            self.read_bool_setting("{}_advanced_visible".format(prefix), False)
        )

        if include_failure_modes:
            # Unsafe checkboxes always start unchecked; see save_advanced_settings.
            getattr(self, "{}_reverse_penalty_edit".format(prefix)).setText(
                self.read_setting("{}_reverse_penalty".format(prefix), "")
            )
            getattr(self, "{}_illegal_turn_penalty_edit".format(prefix)).setText(
                self.read_setting("{}_illegal_turn_penalty".format(prefix), "")
            )
            getattr(
                self, "{}_ignored_restriction_penalty_edit".format(prefix)
            ).setText(self.read_setting("{}_ignored_restriction_penalty".format(prefix), ""))
            getattr(self, "{}_forbidden_uturn_penalty_edit".format(prefix)).setText(
                self.read_setting("{}_forbidden_uturn_penalty".format(prefix), "")
            )
            getattr(self, "{}_max_illegal_distance_edit".format(prefix)).setText(
                self.read_setting("{}_max_illegal_distance".format(prefix), "")
            )
            getattr(self, "{}_max_illegal_turns_edit".format(prefix)).setText(
                self.read_setting("{}_max_illegal_turns".format(prefix), "")
            )
            getattr(self, "{}_alternative_count_edit".format(prefix)).setText(
                self.read_setting("{}_alternative_count".format(prefix), "1")
            )
            getattr(self, "{}_alternative_cost_ratio_edit".format(prefix)).setText(
                self.read_setting("{}_alternative_cost_ratio".format(prefix), "1.35")
            )
            getattr(self, "{}_alternative_min_jaccard_edit".format(prefix)).setText(
                self.read_setting("{}_alternative_min_jaccard".format(prefix), "0.2")
            )
            getattr(self, "{}_unsafe_toggle".format(prefix)).setChecked(
                self.read_bool_setting("{}_unsafe_visible".format(prefix), False)
            )
