//! Narrow checker operations used by Effect's declared API stability analysis.
//! Contextual replay deliberately retains the already selected signatures: an
//! `IgnoreNodeInferences` query would resolve calls again and can change the
//! program's diagnostics while inspecting a property declaration.

use crate::*;

struct StabilityInferenceLevel {
    call: P<Node>,
    target: P<Signature>,
    arguments: Vec<P<Node>>,
    index: usize,
    this_type: Option<P<Type>>,
    argument_types: Vec<Option<P<Type>>>,
    fixed: Option<P<Type>>,
}

impl Checker {
    pub fn effect_same_symbol_reference(&mut self, left: P<Symbol>, right: P<Symbol>) -> bool {
        self.get_symbol_if_same_reference(left, right).is_some()
    }

    /// One import/export hop, preserving tags on forwarding declarations and
    /// avoiding both synthetic default aliases and cached failed resolutions.
    pub fn effect_immediate_aliased_symbol(&mut self, symbol: P<Symbol>) -> Option<P<Symbol>> {
        if !symbol.flags().intersects(SymbolFlags::Alias)
            || !symbol.declarations().iter().copied().any(ast::is_alias_symbol_declaration)
            || self
                .alias_symbol_links
                .try_get(symbol)
                .is_some_and(|links| links.alias_target.get() == Some(self.unknown_symbol))
        {
            return None;
        }
        self.get_immediate_aliased_symbol(symbol)
    }

    pub fn effect_object_literal_constraint_contextual_type(
        &mut self,
        node: P<Node>,
    ) -> Option<P<Type>> {
        if node.kind() != Kind::ObjectLiteralExpression {
            return None;
        }
        let levels = self.stability_inference_replay_levels(node);
        if levels.is_empty() {
            return None;
        }
        let mut pushed = 0;
        let mut complete = true;
        for level in levels.iter().rev() {
            let Some(argument_type) = self.stability_argument_type_without_own_inferences(level)
            else {
                complete = false;
                break;
            };
            self.push_contextual_type(level.arguments[level.index], Some(argument_type), false);
            pushed += 1;
        }
        let result =
            if complete { self.get_contextual_type(node, ContextFlags::None) } else { None };
        for _ in 0..pushed {
            self.pop_contextual_type();
        }
        result
    }

    pub fn effect_object_literal_bare_constraint_contextual_type(
        &mut self,
        node: P<Node>,
    ) -> Option<P<Type>> {
        if node.kind() != Kind::ObjectLiteralExpression {
            return None;
        }
        let (call, arguments, index) = self.stability_containing_call_argument(node)?;
        let signature = self.get_resolved_signature(call, None, CheckMode::Normal);
        let target = signature.target()?;
        let parameters = target.type_parameters();
        let arguments_types = self.get_type_arguments_for_resolved_signature(signature);
        if parameters.is_empty() || arguments_types.len() != parameters.len() {
            return None;
        }
        let inferred = new_type_mapper(parameters, alloc_vec(arguments_types));
        let mut constrained = Vec::with_capacity(parameters.len());
        let mut has_constraint = false;
        for &parameter in parameters {
            if let Some(constraint) = self.get_constraint_of_type_parameter(parameter) {
                constrained.push(self.instantiate_type(constraint, Some(inferred)));
                has_constraint = true;
            } else {
                constrained.push(self.unknown_type);
            }
        }
        if !has_constraint {
            return None;
        }
        let parameter_type = self.get_type_at_position(target, index);
        let mapper = new_type_mapper(parameters, alloc_vec(constrained));
        let argument_type = self.instantiate_type(parameter_type, Some(mapper));
        self.push_contextual_type(arguments[index], Some(argument_type), false);
        let result = self.get_contextual_type(node, ContextFlags::None);
        self.pop_contextual_type();
        result
    }

    fn stability_inference_replay_levels(&mut self, node: P<Node>) -> Vec<StabilityInferenceLevel> {
        let mut levels = Vec::new();
        let mut current = node;
        while let Some((call, arguments, index)) = self.stability_containing_call_argument(current)
        {
            let signature = self.get_resolved_signature(call, None, CheckMode::Normal);
            let Some(target) =
                signature.target().filter(|target| !target.type_parameters().is_empty())
            else {
                break;
            };
            if !call.type_arguments().is_empty() {
                let fixed = self.get_type_at_position(signature, index);
                levels.push(StabilityInferenceLevel {
                    call,
                    target,
                    arguments,
                    index,
                    this_type: None,
                    argument_types: Vec::new(),
                    fixed: Some(fixed),
                });
                break;
            }
            let this_type = if self.get_this_type_of_signature(target).is_some() {
                let this_argument = self.get_this_argument_of_call(call);
                Some(self.get_this_argument_type(this_argument))
            } else {
                None
            };
            let argument_types = arguments
                .iter()
                .enumerate()
                .map(|(i, &argument)| {
                    (i != index && argument.kind() != Kind::OmittedExpression)
                        .then(|| self.check_expression_cached(argument))
                })
                .collect();
            levels.push(StabilityInferenceLevel {
                call,
                target,
                arguments,
                index,
                this_type,
                argument_types,
                fixed: None,
            });
            current = call;
        }
        levels
    }

    fn stability_argument_type_without_own_inferences(
        &mut self,
        level: &StabilityInferenceLevel,
    ) -> Option<P<Type>> {
        if level.fixed.is_some() {
            return level.fixed;
        }
        let parameters = level.target.type_parameters();
        let context =
            Self::new_inference_context(parameters, Some(level.target), InferenceFlags::None, None);
        let inferences = context.inferences.get();
        let skip_binding_patterns = parameters
            .iter()
            .all(|&parameter| self.get_default_from_type_parameter(parameter).is_some());
        let flags = if skip_binding_patterns {
            ContextFlags::SkipBindingPatterns
        } else {
            ContextFlags::None
        };
        if let Some(contextual) = self.get_contextual_type(level.call, flags) {
            let is_binding_pattern = !skip_binding_patterns
                && self.get_contextual_type(level.call, ContextFlags::SkipBindingPatterns)
                    != Some(contextual);
            if !is_binding_pattern {
                let mut source = contextual;
                if let Some(signature) = self
                    .get_single_call_signature(contextual)
                    .filter(|s| !s.type_parameters().is_empty())
                {
                    let instantiated = self
                        .get_signature_instantiation_without_filling_in_type_arguments(
                            signature,
                            signature.type_parameters(),
                        );
                    source = self.get_or_create_type_from_signature(instantiated);
                }
                let returns = self.get_return_type_of_signature(level.target);
                self.infer_types(
                    inferences,
                    source,
                    returns,
                    InferencePriority::RETURN_TYPE,
                    false,
                );
            }
        }
        if let Some(this_type) = level.this_type
            && let Some(parameter_type) = self.get_this_type_of_signature(level.target)
        {
            self.infer_types(inferences, this_type, parameter_type, InferencePriority::NONE, false);
        }
        for (index, argument_type) in level.argument_types.iter().enumerate() {
            if let Some(argument_type) = *argument_type {
                let parameter_type = self.get_type_at_position(level.target, index);
                self.infer_types(
                    inferences,
                    argument_type,
                    parameter_type,
                    InferencePriority::NONE,
                    false,
                );
            }
        }
        let parameter_type = self.get_type_at_position(level.target, level.index);
        Some(self.instantiate_type(parameter_type, Self::get_mapper_from_context(Some(context))))
    }

    fn stability_containing_call_argument(
        &mut self,
        node: P<Node>,
    ) -> Option<(P<Node>, Vec<P<Node>>, usize)> {
        let mut current = node;
        let call = loop {
            let parent = current.parent()?;
            if ast::is_call_like_expression(parent) {
                if ast::is_call_or_new_expression(parent) && parent.expression() != Some(current)
                    || parent.kind() == Kind::TaggedTemplateExpression
                        && parent.as_tagged_template_expression().tag() != current
                {
                    break parent;
                }
                return None;
            }
            current = parent;
        };
        let arguments = self.get_effective_call_arguments(call);
        let mut argument = node;
        while argument != call && !arguments.contains(&argument) {
            argument = argument.parent()?;
        }
        let index = arguments.iter().position(|&candidate| candidate == argument)?;
        if arguments[..=index].iter().any(|argument| argument.kind() == Kind::SpreadElement) {
            return None;
        }
        Some((call, arguments, index))
    }
}
