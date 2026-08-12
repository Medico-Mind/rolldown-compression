use simd_brotli::enc::threading::{ScopeBody, ScopedSpawner, ThreadScope};

pub struct RayonThreadScope;

struct RayonSpawner<'a, 'scope>(&'a rayon::Scope<'scope>);

impl<'a, 'scope, 'env: 'scope> ScopedSpawner<'env> for RayonSpawner<'a, 'scope> {
    fn spawn<Task: FnOnce() + Send + 'env>(&self, task: Task) {
        self.0.spawn(move |_| task());
    }
}

impl ThreadScope for RayonThreadScope {
    fn scope<'env, Body: ScopeBody<'env>>(&self, body: Body) -> Body::Output {
        rayon::scope(|scope| body.run(&RayonSpawner(scope)))
    }
}
