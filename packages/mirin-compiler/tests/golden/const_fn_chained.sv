module chain #(parameter int N) (
    input  logic [7:0] x,
    output logic [7:0] result
);
    function automatic int sum_to(input int n);
        int acc;
        acc = 0;
        for (int i = 0; i < n; i++) begin
            acc = (acc + i);
        end
        return acc;
    endfunction
    localparam int a = sum_to(N);
    localparam int b = sum_to(a);
    logic [a-1:0] t1;
    logic [b-1:0] t2;
    assign t1 = (a'(x));
    assign t2 = (b'(t1));
    assign result = (8'(t2));
endmodule
