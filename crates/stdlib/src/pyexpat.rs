//! Pyexpat builtin module

// false positive: core::io::Cursor is unstable (core_io), unusable on stable
#![expect(clippy::std_instead_of_core)]

// spell-checker: ignore libexpat

pub(crate) use _pyexpat::module_def;

macro_rules! create_property {
    ($ctx: expr, $attributes: expr, $name: expr, $class: expr, $element: ident) => {
        let attr = $ctx.new_static_getset(
            $name,
            $class,
            move |this: &PyExpatLikeXmlParser| this.$element.read().clone(),
            move |this: &PyExpatLikeXmlParser, func: PyObjectRef| *this.$element.write() = func,
        );

        $attributes.insert($ctx.intern_str($name), attr.into());
    };
}

macro_rules! create_bool_property {
    ($ctx: expr, $attributes: expr, $name: expr, $class: expr, $element: ident) => {
        let attr = $ctx.new_static_getset(
            $name,
            $class,
            move |this: &PyExpatLikeXmlParser| this.$element.read().clone(),
            move |this: &PyExpatLikeXmlParser,
                  value: PyObjectRef,
                  vm: &VirtualMachine|
                  -> PyResult<()> {
                let bool_value = value.is_true(vm)?;
                *this.$element.write() = vm.ctx.new_bool(bool_value).into();
                Ok(())
            },
        );

        $attributes.insert($ctx.intern_str($name), attr.into());
    };
}

macro_rules! create_int_property {
    ($ctx: expr, $attributes: expr, $name: expr, $class: expr, $element: ident) => {
        let attr = $ctx.new_static_getset(
            $name,
            $class,
            move |this: &PyExpatLikeXmlParser| -> usize { *this.$element.read() },
            move |this: &PyExpatLikeXmlParser,
                  value: PyObjectRef,
                  vm: &VirtualMachine|
                  -> PyResult<()> {
                let int = value
                    .downcast_ref::<PyInt>()
                    .ok_or_else(|| vm.new_type_error(format!("{} must be an int", $name)))?;
                // expat stores the buffer size as a C ssize_t; reject values
                // that overflow it and non-positive values.
                let size = int.try_to_primitive::<i64>(vm).map_err(|_| {
                    vm.new_value_error(format!("{} must be a positive integer", $name))
                })?;
                if size <= 0 {
                    return Err(vm.new_value_error(format!(
                        "{} must be a positive integer",
                        $name
                    )));
                }
                *this.$element.write() = size as usize;
                Ok(())
            },
        );

        $attributes.insert($ctx.intern_str($name), attr.into());
    };
}

#[pymodule(name = "pyexpat")]
mod _pyexpat {
    use crate::vm::{
        AsObject, Context, Py, PyObjectRef, PyPayload, PyRef, PyResult, TryFromObject,
        VirtualMachine,
        builtins::{PyBytesRef, PyException, PyInt, PyModule, PyStr, PyStrRef, PyType, PyUtf8StrRef},
        extend_module,
        function::{ArgBytesLike, ArgPrimitiveIndex, Either, IntoFuncArgs, OptionalArg},
        types::Constructor,
    };
    use rustpython_common::lock::PyRwLock;

    /// xml-rs only recognizes a handful of encoding labels; expat accepts many
    /// aliases (e.g. `iso8859` for ISO-8859-1). Rewrite the XML declaration so
    /// xml-rs can decode the document. The prolog is ASCII, so byte offsets
    /// from the lossy decode line up with the original buffer.
    fn normalize_encoding_decl(bytes: &[u8]) -> Vec<u8> {
        let head = &bytes[..bytes.len().min(256)];
        let s = String::from_utf8_lossy(head);
        let Some(rel) = s.find("encoding") else {
            return bytes.to_vec();
        };
        let after = &s[rel + "encoding".len()..];
        let after_eq = after.trim_start().strip_prefix('=').unwrap_or(after.trim_start());
        let trimmed = after_eq.trim_start();
        let quote = trimmed.chars().next().filter(|&c| c == '"' || c == '\'');
        let Some(quote) = quote else {
            return bytes.to_vec();
        };
        // Number of bytes consumed between "encoding" and the value quote
        // (leading spaces, '=', leading spaces).
        let consumed = after.len() - trimmed.len();
        let val_start = rel + "encoding".len() + consumed + quote.len_utf8();
        let val = &trimmed[quote.len_utf8()..];
        let Some(close) = val.find(quote) else {
            return bytes.to_vec();
        };
        let val_end = val_start + close;
        let mapped = match s[val_start..val_end].to_ascii_lowercase().as_str() {
            // xml-rs routes any declared encoding through a byte-decoder that
            // buffers text runs, so trailing text is lost when the stream ends
            // mid-document (incremental Parse). Bytes are passed through as
            // UTF-8 regardless, so map ASCII-compatible aliases to utf-8 to
            // keep the fast no-decoder path.
            "iso8859" | "iso8859-1" | "iso-8859_1" | "iso_8859_1" | "latin1" | "ascii"
            | "usascii" | "us_ascii" | "utf8" => "utf-8",
            _ => return bytes.to_vec(),
        };
        let mut out = bytes.to_vec();
        out.splice(val_start..val_end, mapped.bytes());
        out
    }
    use std::io::Cursor;
    use xml::reader::XmlEvent;

    pub(crate) fn module_exec(vm: &VirtualMachine, module: &Py<PyModule>) -> PyResult<()> {
        __module_exec(vm, module);

        // Add submodules
        let model = super::_model::module_def(&vm.ctx).create_module(vm)?;
        let errors = super::_errors::module_def(&vm.ctx).create_module(vm)?;

        extend_module!(vm, module, {
            "model" => model,
            "errors" => errors,
        });

        Ok(())
    }

    type MutableObject = PyRwLock<PyObjectRef>;

    #[pyattr(name = "version_info")]
    pub(super) const VERSION_INFO: (u32, u32, u32) = (2, 7, 1);

    #[pyattr]
    const XML_PARAM_ENTITY_PARSING_NEVER: i32 = 0;
    #[pyattr]
    const XML_PARAM_ENTITY_PARSING_UNLESS_STANDALONE: i32 = 1;
    #[pyattr]
    const XML_PARAM_ENTITY_PARSING_ALWAYS: i32 = 2;

    #[pyattr]
    #[pyattr(name = "XMLParserType")]
    #[pyclass(name = "xmlparser", module = false, traverse)]
    #[derive(Debug, PyPayload)]
    pub(super) struct PyExpatLikeXmlParser {
        #[pytraverse(skip)]
        namespace_separator: Option<String>,
        #[pytraverse(skip)]
        base: PyRwLock<Option<String>>,
        start_element: MutableObject,
        end_element: MutableObject,
        character_data: MutableObject,
        entity_decl: MutableObject,
        buffer_text: MutableObject,
        #[pytraverse(skip)]
        text_buffer: PyRwLock<String>,
        #[pytraverse(skip)]
        buffer_size: PyRwLock<usize>,
        #[pytraverse(skip)]
        reparse_deferral_enabled: PyRwLock<bool>,
        #[pytraverse(skip)]
        pending_input: PyRwLock<Vec<u8>>,
        namespace_prefixes: MutableObject,
        ordered_attributes: MutableObject,
        specified_attributes: MutableObject,
        intern: MutableObject,
        // Additional handlers (stubs for compatibility)
        processing_instruction: MutableObject,
        unparsed_entity_decl: MutableObject,
        notation_decl: MutableObject,
        start_namespace_decl: MutableObject,
        end_namespace_decl: MutableObject,
        comment: MutableObject,
        start_cdata_section: MutableObject,
        end_cdata_section: MutableObject,
        default: MutableObject,
        default_expand: MutableObject,
        not_standalone: MutableObject,
        external_entity_ref: MutableObject,
        start_doctype_decl: MutableObject,
        end_doctype_decl: MutableObject,
        xml_decl: MutableObject,
        element_decl: MutableObject,
        attlist_decl: MutableObject,
        skipped_entity: MutableObject,
    }
    type PyExpatLikeXmlParserRef = PyRef<PyExpatLikeXmlParser>;

    #[inline]
    fn invoke_handler<T>(vm: &VirtualMachine, handler: &MutableObject, args: T)
    where
        T: IntoFuncArgs,
    {
        // Clone the handler while holding the read lock, then release the lock
        let handler = handler.read().clone();
        handler.call(args, vm).ok();
    }

    #[pyclass]
    impl PyExpatLikeXmlParser {
        fn new(
            namespace_separator: Option<String>,
            intern: Option<PyObjectRef>,
            vm: &VirtualMachine,
        ) -> PyExpatLikeXmlParserRef {
            let intern_dict = intern.unwrap_or_else(|| vm.ctx.new_dict().into());
            Self {
                namespace_separator,
                base: PyRwLock::new(None),
                start_element: MutableObject::new(vm.ctx.none()),
                end_element: MutableObject::new(vm.ctx.none()),
                character_data: MutableObject::new(vm.ctx.none()),
                entity_decl: MutableObject::new(vm.ctx.none()),
                buffer_text: MutableObject::new(vm.ctx.new_bool(false).into()),
                text_buffer: PyRwLock::new(String::new()),
                buffer_size: PyRwLock::new(1024),
                reparse_deferral_enabled: PyRwLock::new(true),
                pending_input: PyRwLock::new(Vec::new()),
                namespace_prefixes: MutableObject::new(vm.ctx.new_bool(false).into()),
                ordered_attributes: MutableObject::new(vm.ctx.new_bool(false).into()),
                specified_attributes: MutableObject::new(vm.ctx.new_bool(false).into()),
                intern: MutableObject::new(intern_dict),
                // Additional handlers (stubs for compatibility)
                processing_instruction: MutableObject::new(vm.ctx.none()),
                unparsed_entity_decl: MutableObject::new(vm.ctx.none()),
                notation_decl: MutableObject::new(vm.ctx.none()),
                start_namespace_decl: MutableObject::new(vm.ctx.none()),
                end_namespace_decl: MutableObject::new(vm.ctx.none()),
                comment: MutableObject::new(vm.ctx.none()),
                start_cdata_section: MutableObject::new(vm.ctx.none()),
                end_cdata_section: MutableObject::new(vm.ctx.none()),
                default: MutableObject::new(vm.ctx.none()),
                default_expand: MutableObject::new(vm.ctx.none()),
                not_standalone: MutableObject::new(vm.ctx.none()),
                external_entity_ref: MutableObject::new(vm.ctx.none()),
                start_doctype_decl: MutableObject::new(vm.ctx.none()),
                end_doctype_decl: MutableObject::new(vm.ctx.none()),
                xml_decl: MutableObject::new(vm.ctx.none()),
                element_decl: MutableObject::new(vm.ctx.none()),
                attlist_decl: MutableObject::new(vm.ctx.none()),
                skipped_entity: MutableObject::new(vm.ctx.none()),
            }
            .into_ref(&vm.ctx)
        }

        #[extend_class]
        fn extend_class_with_fields(ctx: &Context, class: &'static Py<PyType>) {
            let mut attributes = class.attributes.write();

            create_property!(ctx, attributes, "StartElementHandler", class, start_element);
            create_property!(ctx, attributes, "EndElementHandler", class, end_element);
            create_property!(
                ctx,
                attributes,
                "CharacterDataHandler",
                class,
                character_data
            );
            create_property!(ctx, attributes, "EntityDeclHandler", class, entity_decl);
            create_bool_property!(ctx, attributes, "buffer_text", class, buffer_text);
            create_int_property!(ctx, attributes, "buffer_size", class, buffer_size);
            create_bool_property!(
                ctx,
                attributes,
                "namespace_prefixes",
                class,
                namespace_prefixes
            );
            create_bool_property!(
                ctx,
                attributes,
                "ordered_attributes",
                class,
                ordered_attributes
            );
            create_bool_property!(
                ctx,
                attributes,
                "specified_attributes",
                class,
                specified_attributes
            );
            create_property!(ctx, attributes, "intern", class, intern);
            // Additional handlers (stubs for compatibility)
            create_property!(
                ctx,
                attributes,
                "ProcessingInstructionHandler",
                class,
                processing_instruction
            );
            create_property!(
                ctx,
                attributes,
                "UnparsedEntityDeclHandler",
                class,
                unparsed_entity_decl
            );
            create_property!(ctx, attributes, "NotationDeclHandler", class, notation_decl);
            create_property!(
                ctx,
                attributes,
                "StartNamespaceDeclHandler",
                class,
                start_namespace_decl
            );
            create_property!(
                ctx,
                attributes,
                "EndNamespaceDeclHandler",
                class,
                end_namespace_decl
            );
            create_property!(ctx, attributes, "CommentHandler", class, comment);
            create_property!(
                ctx,
                attributes,
                "StartCdataSectionHandler",
                class,
                start_cdata_section
            );
            create_property!(
                ctx,
                attributes,
                "EndCdataSectionHandler",
                class,
                end_cdata_section
            );
            create_property!(ctx, attributes, "DefaultHandler", class, default);
            create_property!(
                ctx,
                attributes,
                "DefaultHandlerExpand",
                class,
                default_expand
            );
            create_property!(
                ctx,
                attributes,
                "NotStandaloneHandler",
                class,
                not_standalone
            );
            create_property!(
                ctx,
                attributes,
                "ExternalEntityRefHandler",
                class,
                external_entity_ref
            );
            create_property!(
                ctx,
                attributes,
                "StartDoctypeDeclHandler",
                class,
                start_doctype_decl
            );
            create_property!(
                ctx,
                attributes,
                "EndDoctypeDeclHandler",
                class,
                end_doctype_decl
            );
            create_property!(ctx, attributes, "XmlDeclHandler", class, xml_decl);
            create_property!(ctx, attributes, "ElementDeclHandler", class, element_decl);
            create_property!(ctx, attributes, "AttlistDeclHandler", class, attlist_decl);
            create_property!(
                ctx,
                attributes,
                "SkippedEntityHandler",
                class,
                skipped_entity
            );
        }

        fn create_config(&self) -> xml::ParserConfig {
            xml::ParserConfig::new()
                .cdata_to_characters(false)
                .coalesce_characters(false)
                .ignore_comments(false)
                .whitespace_to_characters(true)
        }

        #[pymethod(name = "SetParamEntityParsing")]
        fn set_param_entity_parsing(&self, _flag: ArgPrimitiveIndex<i32>) -> i32 {
            // Compatibility shim: xml.sax requires this setup API, but xml-rs
            // does not expose Expat parameter entity parsing configuration.
            1
        }

        #[pymethod(name = "GetReparseDeferralEnabled")]
        fn get_reparse_deferral_enabled(&self) -> bool {
            *self.reparse_deferral_enabled.read()
        }

        #[pymethod(name = "SetReparseDeferralEnabled")]
        fn set_reparse_deferral_enabled(&self, enabled: bool) {
            // Expat >= 2.6 exposes this switch. xml-rs does not have the
            // reparse-deferral algorithm, but preserving the state makes the
            // public API round-trip correctly and lets callers configure it.
            *self.reparse_deferral_enabled.write() = enabled;
        }

        #[pymethod(name = "SetBase")]
        fn set_base(&self, base: PyStrRef) {
            // Store-only compatibility state for xml.sax locator APIs. The
            // xml-rs backend still does not perform Expat-style base URI
            // resolution for external entities.
            *self.base.write() = Some(AsRef::<str>::as_ref(&base).to_owned());
        }

        #[pymethod(name = "GetBase")]
        fn get_base(&self, vm: &VirtualMachine) -> PyObjectRef {
            self.base.read().as_ref().map_or_else(
                || vm.ctx.none(),
                |base| vm.ctx.new_str(base.as_str()).into(),
            )
        }

        /// Construct element name with namespace if separator is set
        fn make_name(&self, name: &xml::name::OwnedName) -> String {
            match (&self.namespace_separator, &name.namespace) {
                (Some(sep), Some(ns)) => format!("{}{}{}", ns, sep, name.local_name),
                _ => name.local_name.clone(),
            }
        }

        fn do_parse<T>(
            &self,
            vm: &VirtualMachine,
            parser: xml::EventReader<T>,
        ) -> Result<(), xml::reader::Error>
        where
            T: std::io::Read,
        {
            // Flush buffered text even when the stream ends mid-document
            // (incremental parse without isfinal), like expat.
            let result = (|| {
                for e in parser {
                    match e? {
                        XmlEvent::StartElement {
                        name, attributes, ..
                    } => {
                        self.flush_if_handler(vm, &self.start_element);
                        let ordered = self.ordered_attributes.read().is(&vm.ctx.true_value);
                        // Build the container.
                        let attrs: PyObjectRef = if ordered {
                            let mut items = Vec::with_capacity(attributes.len() * 2);
                            for attribute in attributes {
                                items.push(vm.ctx.new_str(self.make_name(&attribute.name)).into());
                                items.push(vm.ctx.new_str(attribute.value).into());
                            }
                            vm.ctx.new_list(items).into()
                        } else {
                            let dict = vm.ctx.new_dict();
                            for attribute in attributes {
                                dict.set_item(
                                    self.make_name(&attribute.name).as_str(),
                                    vm.ctx.new_str(attribute.value).into(),
                                    vm,
                                )
                                .unwrap();
                            }
                            dict.into()
                        };

                        let name_str = PyStr::from(self.make_name(&name)).into_ref(&vm.ctx);
                        invoke_handler(vm, &self.start_element, (name_str, attrs));
                    }
                    XmlEvent::EndElement { name, .. } => {
                        self.flush_if_handler(vm, &self.end_element);
                        let name_str = PyStr::from(self.make_name(&name)).into_ref(&vm.ctx);
                        invoke_handler(vm, &self.end_element, (name_str,));
                    }
                    XmlEvent::Characters(chars) => {
                        if self.buffering(vm) {
                            let mut buf = self.text_buffer.write();
                            buf.push_str(&chars);
                            // Expat flushes the buffer as soon as it reaches
                            // buffer_size, even mid-open-element.
                            if buf.len() >= *self.buffer_size.read() {
                                drop(buf);
                                self.flush_text_buffer(vm);
                            }
                        } else {
                            let str = PyStr::from(chars).into_ref(&vm.ctx);
                            invoke_handler(vm, &self.character_data, (str,));
                        }
                    }
                    XmlEvent::ProcessingInstruction { name, data } => {
                        self.flush_if_handler(vm, &self.processing_instruction);
                        let name = PyStr::from(name).into_ref(&vm.ctx);
                        let data = PyStr::from(data.unwrap_or_default()).into_ref(&vm.ctx);
                        invoke_handler(vm, &self.processing_instruction, (name, data));
                    }
                    XmlEvent::Comment(comment) => {
                        self.flush_if_handler(vm, &self.comment);
                        let comment = PyStr::from(comment).into_ref(&vm.ctx);
                        invoke_handler(vm, &self.comment, (comment,));
                    }
                    XmlEvent::CData(chars) => {
                        self.flush_if_handler(vm, &self.start_cdata_section);
                        invoke_handler(vm, &self.start_cdata_section, ());
                        let str = PyStr::from(chars).into_ref(&vm.ctx);
                        invoke_handler(vm, &self.character_data, (str,));
                        invoke_handler(vm, &self.end_cdata_section, ());
                    }
                    _ => {}
                    }
                }
                Ok(())
            })();
            // Flush any remaining buffered text at the end of the parse.
            self.flush_text_buffer(vm);
            result
        }

        fn buffering(&self, vm: &VirtualMachine) -> bool {
            self.buffer_text.read().is(&vm.ctx.true_value)
        }

        /// Expat only flushes the character-data buffer right before a markup
        /// handler that is actually set; otherwise text is collapsed.
        fn flush_if_handler(&self, vm: &VirtualMachine, handler: &MutableObject) {
            if !vm.is_none(&*handler.read()) {
                self.flush_text_buffer(vm);
            }
        }

        /// Flush buffered character data, calling the CharacterDataHandler
        /// with the accumulated text in chunks of at most buffer_size bytes.
        fn flush_text_buffer(&self, vm: &VirtualMachine) {
            let text = core::mem::take(&mut *self.text_buffer.write());
            if text.is_empty() {
                return;
            }
            let size = (*self.buffer_size.read()).max(1);
            let len = text.len();
            let mut start = 0;
            while start < len {
                let mut end = (start + size).min(len);
                while end > start && !text.is_char_boundary(end) {
                    end -= 1;
                }
                let str = PyStr::from(text[start..end].to_owned()).into_ref(&vm.ctx);
                invoke_handler(vm, &self.character_data, (str,));
                start = end;
            }
        }

        fn input_is_incomplete(&self, bytes: &[u8]) -> bool {
            let reader = Cursor::new(normalize_encoding_decl(bytes));
            let parser = self.create_config().create_reader(reader);
            let mut saw_top_level_markup = false;
            let mut saw_element = false;
            for event in parser {
                match event {
                    Ok(xml::reader::XmlEvent::StartElement { .. }) => saw_element = true,
                    Ok(xml::reader::XmlEvent::ProcessingInstruction { .. })
                    | Ok(xml::reader::XmlEvent::Comment(_)) => saw_top_level_markup = true,
                    Ok(_) => {}
                    Err(err) => {
                        let unexpected_eof = match err.kind() {
                            xml::reader::ErrorKind::UnexpectedEof => true,
                            xml::reader::ErrorKind::Syntax(msg) => {
                                msg.contains("Unexpected end of stream")
                            }
                            _ => false,
                        };
                        // A top-level PI/comment is a complete SAX event even
                        // though xml-rs reports EOF because no root element
                        // followed it. Replay it now instead of waiting for a
                        // later feed() call.
                        return unexpected_eof && !(saw_top_level_markup && !saw_element);
                    }
                }
            }
            false
        }

        #[pymethod(name = "Parse")]
        fn parse(
            &self,
            data: Either<PyStrRef, PyBytesRef>,
            isfinal: OptionalArg<bool>,
            vm: &VirtualMachine,
        ) -> i32 {
            let bytes = match data {
                Either::A(s) => s.as_bytes().to_vec(),
                Either::B(b) => b.as_bytes().to_vec(),
            };
            let isfinal = isfinal.unwrap_or(false);

            let mut pending = self.pending_input.write();
            let was_pending = !pending.is_empty();
            pending.extend_from_slice(&bytes);

            if pending.is_empty() {
                return 1;
            }

            if !isfinal {
                let incomplete = self.input_is_incomplete(&pending);
                if incomplete
                    || (was_pending && *self.reparse_deferral_enabled.read())
                {
                    return 1;
                }
            }

            let bytes = core::mem::take(&mut *pending);
            drop(pending);
            let bytes = normalize_encoding_decl(&bytes);
            let reader = Cursor::<Vec<u8>>::new(bytes);
            let parser = self.create_config().create_reader(reader);
            // Note: xml-rs is stricter than libexpat; some errors are silently ignored
            // to maintain compatibility with existing Python code.
            let _ = self.do_parse(vm, parser);
            1
        }

        #[pymethod(name = "ParseFile")]
        fn parse_file(&self, file: PyObjectRef, vm: &VirtualMachine) -> PyResult<i32> {
            let read_res = vm.call_method(&file, "read", ())?;
            let bytes_like = ArgBytesLike::try_from_object(vm, read_res)?;
            let buf = bytes_like.borrow_buf().to_vec();
            if buf.is_empty() {
                return Ok(1);
            }
            let buf = normalize_encoding_decl(&buf);
            let reader = Cursor::new(buf);
            let parser = self.create_config().create_reader(reader);
            // Note: xml-rs is stricter than libexpat; some errors are silently ignored
            let _ = self.do_parse(vm, parser);
            Ok(1)
        }
    }

    #[derive(FromArgs)]
    struct ParserCreateArgs {
        #[pyarg(any, optional)]
        encoding: Option<PyStrRef>,
        #[pyarg(any, optional)]
        namespace_separator: Option<PyUtf8StrRef>,
        #[pyarg(any, optional)]
        intern: Option<PyObjectRef>,
    }

    #[pyfunction(name = "ParserCreate")]
    fn parser_create(
        args: ParserCreateArgs,
        vm: &VirtualMachine,
    ) -> PyResult<PyExpatLikeXmlParserRef> {
        // Validate namespace_separator: must be at most one character
        let ns_sep = match args.namespace_separator {
            Some(ref s) => {
                if s.as_str().chars().count() > 1 {
                    return Err(vm.new_value_error(
                        "namespace_separator must be at most one character, omitted, or None",
                    ));
                }
                Some(s.as_str().to_owned())
            }
            None => None,
        };

        // encoding parameter is currently not used (xml-rs handles encoding from XML declaration)
        let _ = args.encoding;

        Ok(PyExpatLikeXmlParser::new(ns_sep, args.intern, vm))
    }

    // TODO: Tie this exception to the module's state.
    #[pyattr]
    #[pyattr(name = "error")]
    #[pyexception(name = "ExpatError", base = PyException)]
    #[derive(Debug)]
    #[repr(transparent)]
    pub(super) struct PyExpatError(PyException);

    #[pyexception]
    impl PyExpatError {}
}

#[pymodule(name = "model")]
mod _model {}

#[pymodule(name = "errors")]
mod _errors {}
