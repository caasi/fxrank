fn free_fn() {}
struct S;
impl S { fn method(&self) {} }
trait T { fn defaulted(&self) {} fn required(&self); }
impl T for S { fn defaulted(&self) {} fn required(&self) {} }
