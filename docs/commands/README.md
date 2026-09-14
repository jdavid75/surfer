# Commands

To execute a command, press space and type the command. There is fuzzy match support, so it is enough to type parts of the command name and it will display options that matches.

It is also possible to create a command file, extension `.sucl`, and run that. Running a command file can be done from within Surfer using the menu option in the File menu, through the toolbar button, or by typing the command ``run_command_file``. It can also be done using the ``--command-file`` argument when starting Surfer.

Not all commands are available unless a file is loaded. Also, some commands are not available in the WASM-build (browser/VS Code extension).

## Waveform/transaction loading and reloading

* ``load_file <FILE_NAME>``

    Load a file. Note that it works to load a waveform file from a command file.

    <div class="warning">In WASM-builds (web browser/VS Code plugin) it is not possible to open a file due to file access restrictions. Use <tt>load_url</tt>.</div>


* ``switch_file <FILE_NAME>``

    Load file, but keep waveform view.

* ``load_url <URL>``

    Load a URL.

* ``reload``

    Reload the current file. Does not work in a web browser.

* ``remove_unavailable``

    Remove variables that are not longer present in the reloaded/switched file.

## Add variable/transaction items

* ``scope_add <SCOPE_NAME>``, ``stream_add``, ``module_add``

    Add all variables in the specified scope to the waveform display. ``module_add`` is an alias for ``scope_add``.

* ``scope_add_recursive <SCOPE_NAME>``

    Add all variables in the specified scope and from all sub-scopes to the waveform display.

    <div class="warning">Adding large hierarchies with a large number of variables can freeze surfer for a significant amount of time.</div>

* ``scope_add_as_group <SCOPE_NAME>``

    Add all variables in the specified scope to the waveform display in a newly created group of the same name.

* ``scope_add_as_group_recursive <SCOPE_NAME>``

    Add all variables in the specified scope and all sub-scopes to the waveform display in a newly created groups nested.

    <div class="warning">Adding large hierarchies with a large number of variables can freeze surfer for a significant amount of time.</div>

* ``variable_add <FULL_VARIABLE_NAME>``, ``generator_add  <FULL_GENERATOR_NAME>``

    Add a variable/generator using the full path, including scopes/streams.

* ``scope_select <SCOPE_NAME>``, ``stream_select <STREAM_NAME>``

    Select a scope/stream to be active (shown in the side panel).

* ``scope_select_root``, ``stream_select_root``

    Deselect the active scope/stream (resets to the root).

* ``variable_add_from_scope <VARIABLE_NAME>``, ``generator_add_from_stream <GENERATOR_NAME>``

    Add variable/generator from currently selected scope/stream.

## Add other items

* ``divider_add [NAME]``

  Add a divider with the optional given name.

* ``timeline_add``

  Add a timeline row.

* ``decoder_add <DECODER> <INPUT>... [SETTING=VALUE]...``

  Add a decoder to the waveform. Positional arguments after the decoder name are assigned to its inputs in the order the decoder declares them; optional inputs may be omitted. Settings are given as ``key=value``, and the display can be controlled with ``samples=on|off``, ``format=dec|hex`` and ``analog=off|step|interpolated``. For example ``decoder_add spdif tb.data`` or ``decoder_add tdm_audio tb.bitclk tb.frame_sync tb.data channels=2 bits=16``. See [Signal decoders](../plugins/signal_decoders.md) for the available decoders, their inputs, and their settings.

## Groups

* ``group_marked [NAME]``

    Group the currently selected items into a new group with the optional given name.

* ``group_dissolve``

  Remove the focused group, moving its contents to the parent level.

* ``group_fold_recursive``

  Collapse the focused group and all nested groups.

* ``group_unfold_recursive``

  Expand the focused group and all nested groups.

* ``group_fold_all``

  Collapse all groups.

* ``group_unfold_all``

  Expand all groups.

## Controlling item appearance

* ``item_focus <ITEM>``

  Set keyboard focus to the given item (referenced by its alphabetical index shown in the display).

* ``item_set_color <COLOR_NAME>``

  Set the foreground color of the focused item.

* ``item_set_background_color <COLOR_NAME>``

  Set the background color of the focused item.

* ``item_set_format <FORMAT_NAME>``

  Set the value display format of the focused item (e.g. ``hex``, ``binary``, ``decimal``, ``signed``).

* ``item_unset_color``

  Reset to default color.

* ``item_unset_background_color``

  Reset to default background color.

* ``item_unfocus``

  Remove focus from currently focused item.

* ``item_rename <NAME>``

  Rename the currently focused item.

* ``item_set_height <HEIGHT>``

  Set the height of the currently focused item.

* ``item_set_analog off | step | interpolated``

  Set the analog display mode of the currently focused item.

* ``item_set_samples on | off``

  Show or hide the sample values of the currently focused decoder item.

* ``item_set_value_format decimal | hex``

  Set the sample value format of the currently focused decoder item.

* ``theme_select <THEME_NAME>``

  Switch to the given color theme.

## Navigation

* ``zoom_fit``

  Zoom to display the full simulation.

* ``zoom_in``

  Zoom in on the waveform.

* ``zoom_out``

  Zoom out of the waveform.

* ``scroll_to_start``, ``goto_start``

  Scroll to the beginning of the simulation.

* ``scroll_to_end``, ``goto_end``

  Scroll to the end of the simulation.

* ``goto_time <TIME>``

  Center the view at the given time without moving the cursor. ``TIME`` can be a plain integer (raw timescale ticks) or a value with a time unit, e.g. ``100ns``, ``1.5 ms``, ``2us``.

* ``zoom_to <START_TIME> <END_TIME>``

  Zoom the view to the given time range. ``START_TIME`` and ``END_TIME`` can be plain integers (raw timescale ticks) or values with a time unit, e.g. ``100ns``, ``1.5 ms``, ``2us``.

* ``transition_next``

  Move cursor to next transition of focused item. Scroll if not visible.

* ``transition_previous``

  Move cursor to previous transition of focused item. Scroll if not visible.

* ``transaction_next``

  Move to the next transaction of the focused item.

* ``transaction_prev``

  Move to the previous transaction of the focused item.

## UI control

* ``show_controls``

  Show keyboard shortcut help window.

* ``show_mouse_gestures``

  Show mouse gesture help window.

* ``show_quick_start``

  Show the quick start guide window.

* ``show_logs``

  Show log window.

* ``toggle_menu``

  Toggle visibility of menu. If not visible, there will be a burger menu in the toolbar.

* ``toggle_side_panel``

  Toggle visibility of the side panel, i.e., where the scopes and variables are shown.

* ``toggle_fullscreen``

  Toggle fullscreen view.

* ``toggle_tick_lines``

  Toggle display of vertical tick lines on the waveform.

* ``toolbar_set_visible <GROUP> <true | false>``

  Set visibility override for a toolbar group.

  ``GROUP`` accepts a group id. Group ids are:
  ``menu``, ``files``, ``copy``, ``zoom``, ``navigation``, ``transitions``,
  ``add_items``, ``viewports``, ``undo``, ``cxxrtl``, ``time``, ``annotations``.

* ``toolbar_set_row <GROUP> <ROW>``

  Move a toolbar group to the given row (`ROW` is an unsigned integer from `0` to `255`).

  ``GROUP`` accepts a group id.

* ``variable_set_name_type <Local | Unique | Global>``

  Set the name display style for the focused variable.

* ``variable_force_name_type <Local | Unique | Global>``

  Set the name display style for all variables.

* ``preference_set_clock_highlight <Line | Cycle | None>``

  Set how clock signals are highlighted: ``Line`` draws a vertical line, ``Cycle`` shades alternating cycles, ``None`` disables highlighting.

* ``preference_set_hierarchy_style <Separate | Tree>``

  Set how the design hierarchy is shown: ``Separate`` shows scopes and variables in separate panes, ``Tree`` shows them together as a tree.

* ``preference_set_arrow_key_bindings <Edge | Scroll>``

  Set whether arrow keys move to the next/previous signal edge (``Edge``) or scroll the view (``Scroll``).

* ``config_reload``

  Reload the configuration file.

* ``create_default_config``

  Create a default configuration file in the user config directory. See the log for the location of the created file.

## Cursor and markers

* ``goto_cursor``

  Go to the location of the main cursor. If off screen, scroll to it.

* ``goto_marker <MARKER_NAME> | #<MARKER_NUMBER>``

  Go to the location of the given marker. If off screen, scroll to it.

* ``cursor_set <TIME>``

  Move cursor to given time and scroll to it if not in view. ``TIME`` can be a plain integer (raw timescale ticks) or a value with a time unit, e.g. ``100ns``, ``1.5 ms``, ``2us``.

* ``marker_set  <MARKER_NAME> | #<MARKER_NUMBER>``

  Add/set marker to location of cursor.

* ``marker_set_at <TIME> <MARKER_NAME> | #<MARKER_NUMBER>``

  Add/set marker at the given time. ``TIME`` can be a plain integer (raw timescale ticks) or a value with a time unit, e.g. ``100ns``, ``1.5 ms``, ``2us``.

* ``marker_remove <MARKER_NAME> | #<MARKER_NUMBER>``

  Remove marker.

* ``show_marker_window``

  Display window with markers and differences between markers

## Frame buffer

* ``frame_buffer_set_array <SCOPE_NAME>`` / ``frame_buffer_set_variable <VARIABLE_NAME>``

  Set the data source for the frame buffer. Use ``frame_buffer_set_array`` to source pixel data
  from a memory array (a scope), or ``frame_buffer_set_variable`` to source it from a single
  variable.

* ``frame_buffer_set_mode <grayscale | rgb | ycbcr> <BITS> [BITS2 BITS3]``

  Set the color mode and bit widths used when decoding pixels.

  * ``grayscale <BITS>`` — each pixel is a single grey value of `BITS` bits (1–8).
  * ``rgb <R_BITS> <G_BITS> <B_BITS>`` — each pixel is packed as red/green/blue with the given bit widths (each 0–8).
  * ``ycbcr <Y_BITS> <CB_BITS> <CR_BITS>`` — each pixel is packed as Y/Cb/Cr (BT.601) with the given bit widths (each 0–8).

  Examples:

  ```
  frame_buffer_set_mode grayscale 8
  frame_buffer_set_mode rgb 5 6 5
  frame_buffer_set_mode ycbcr 8 8 8
  ```

* ``frame_buffer_set_width <WIDTH>``

  Set the number of pixels per row in the frame buffer display.

* ``frame_buffer_set_range <FIRST> <LAST> [FIRST2 LAST2 ...]``

  Set the displayed index range for each array level. Pairs of integers are matched to levels
  in order; extra pairs beyond the number of levels are ignored. Each value is clamped to the
  valid range of its level, and if `FIRST` > `LAST` the values are swapped automatically.

  Example — set level 0 to rows 0–479 and level 1 to columns 0–639:

  ```
  frame_buffer_set_range 0 479 0 639
  ```

## Memory viewer

* ``memory_viewer_open <SCOPE_NAME>``

  Open a memory viewer window for the given array (scope).

## Viewports

* ``viewport_add``

  Add a new viewport (additional waveform view pane).

* ``viewport_remove``

  Remove the most recently added viewport.

* ``viewport_set_active <INDEX>``

  Set the active viewport by zero-based index.
  Command completion suggests currently available viewport indices.
  If ``INDEX`` is larger than the largest available viewport index,
  the active viewport is set to that largest available index instead.

## State files

* ``load_state <FILE_NAME>``

  Load a previously saved state file.

* ``save_state``

  Save the current state to the default state file.

* ``save_state_as <FILE_NAME>``

  Save the current state to the given file.

## Command files

* ``run_command_file <FILE_NAME>`` (not on WASM)

    Run the commands in the given file.

    <div class="warning">In WASM-builds (web browser/VS Code plugin) it is not possible to run another command file from a command file due to file access restrictions.</div>

* ``run_command_file_from_url <URL>``

    Run the commands at the given URL.

## Surver (streaming waveform server)

* ``surver_select_file <FILE_NAME>``

  Load a file from the connected Surver instance, discarding the current waveform view.

* ``surver_switch_file <FILE_NAME>``

  Load a file from the connected Surver instance, keeping the current waveform view.

## Waveform control protocol (WCP)

* ``wcp_server_start`` (not WASM)

  Start the [WCP](https://gitlab.com/waveform-control-protocol/wcp/) server.
  Typically, this is using port 54321 at address 127.0.0.1, but this can be changed
  using the `address` setting in the `wcp` part of the config file.

* ``wcp_server_stop`` (not WASM)

  Stop the WCP server.

## Other

* ``copy_value``

  Copy the variable name and value at cursor to the clipboard.

* ``undo``

  Undo the last action.

* ``redo``

  Redo the last undone action.

* ``exit`` (not WASM)

  Exit Surfer.

## Interactive simulation

* ``pause_simulation``

  Pause a running simulation.

* ``unpause_simulation``

  Resume a paused simulation.

## Debugging

* ``dump_tree``

  Print the current displayed item tree to the log.

* ``show_performance`` (performance_plot feature only)

  Show the performance plot window. Pass ``redraw`` to also enable continuous redraw mode.
